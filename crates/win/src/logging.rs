use std::collections::VecDeque;
use std::fmt;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::SystemTime;

use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::{
    EnvFilter, fmt as fmt_layer, layer::SubscriberExt, util::SubscriberInitExt,
};

/// One tracing event as the diagnostics page shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EventRecord {
    pub at: SystemTime,
    pub level: Level,
    pub target: String,
    pub message: String,
}

/// The last [`RecentEvents::CAP`] events that passed the active filter,
/// oldest first. Cloning shares the buffer.
#[derive(Clone, Default)]
pub struct RecentEvents(Arc<Mutex<VecDeque<EventRecord>>>);

impl RecentEvents {
    pub const CAP: usize = 50;

    /// Copies the buffer out and releases the lock before returning, so the
    /// caller can emit tracing or build widgets without holding it.
    pub fn snapshot(&self) -> Vec<EventRecord> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .cloned()
            .collect()
    }

    fn push(&self, record: EventRecord) {
        let mut events = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        events.push_back(record);
        while events.len() > RecentEvents::CAP {
            events.pop_front();
        }
    }
}

impl<S: Subscriber> Layer<S> for RecentEvents {
    fn on_event(&self, event: &Event<'_>, _cx: Context<'_, S>) {
        let mut fields = Fields::default();
        event.record(&mut fields);
        let meta = event.metadata();
        self.push(EventRecord {
            at: SystemTime::now(),
            level: *meta.level(),
            target: meta.target().to_owned(),
            message: fields.into_message(),
        });
    }
}

/// Collects the `message` field and appends every other field as `k=v`.
#[derive(Default)]
struct Fields {
    message: String,
    extra: Vec<String>,
}

impl Fields {
    fn into_message(self) -> String {
        let mut message = self.message;
        for field in self.extra {
            if !message.is_empty() {
                message.push(' ');
            }
            message.push_str(&field);
        }
        message
    }

    fn record(&mut self, field: &Field, value: String) {
        // `tracing-log` attaches the `log` record's origin as `log.*` fields;
        // `target` already carries what the page needs.
        if field.name().starts_with("log.") {
            return;
        }
        if field.name() == "message" {
            self.message = value;
        } else {
            self.extra.push(format!("{}={value}", field.name()));
        }
    }
}

impl Visit for Fields {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        self.record(field, format!("{value:?}"));
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        self.record(field, value.to_owned());
    }
}

/// What [`init`] hands back. Keep `guard` alive for the life of the
/// process; dropping it flushes and stops the file writer.
pub struct Logging {
    pub guard: WorkerGuard,
    pub recent: RecentEvents,
}

/// Installs the global subscriber: a daily-rolling file in `logs`, the
/// in-memory ring buffer, plus stderr in debug builds. The filter sits first
/// so every layer sees the same events.
pub fn init(logs: &Path) -> Logging {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let (writer, guard) =
        tracing_appender::non_blocking(tracing_appender::rolling::daily(logs, "ryuuji.log"));
    let file = fmt_layer::layer().with_ansi(false).with_writer(writer);
    let recent = RecentEvents::default();
    let registry = tracing_subscriber::registry()
        .with(filter)
        .with(file)
        .with(recent.clone());

    #[cfg(debug_assertions)]
    let registry = registry.with(fmt_layer::layer().with_writer(std::io::stderr));

    registry.init();
    Logging { guard, recent }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_buffer_keeps_the_last_fifty_in_order() {
        let recent = RecentEvents::default();
        let subscriber = tracing_subscriber::registry().with(recent.clone());
        let _default = subscriber.set_default();

        for i in 0..60 {
            tracing::info!(n = i, "event {i}");
        }

        let events = recent.snapshot();
        assert_eq!(events.len(), RecentEvents::CAP);
        assert_eq!(events[0].message, "event 10 n=10");
        assert_eq!(events[49].message, "event 59 n=59");
        assert!(events.iter().all(|event| event.level == Level::INFO));
        assert!(
            events
                .iter()
                .all(|event| event.target.ends_with("logging::tests"))
        );
        assert!(events.windows(2).all(|pair| pair[0].at <= pair[1].at));
    }

    #[test]
    fn the_env_filter_gates_the_ring_buffer() {
        let recent = RecentEvents::default();
        let subscriber = tracing_subscriber::registry()
            .with(EnvFilter::new("warn"))
            .with(recent.clone());
        let _default = subscriber.set_default();

        tracing::info!("hidden");
        tracing::warn!(reason = "disk", "shown");
        tracing::error!("also shown");

        let messages: Vec<(Level, String)> = recent
            .snapshot()
            .into_iter()
            .map(|event| (event.level, event.message))
            .collect();
        assert_eq!(
            messages,
            [
                (Level::WARN, "shown reason=disk".to_owned()),
                (Level::ERROR, "also shown".to_owned()),
            ]
        );
    }

    #[test]
    fn log_bridge_fields_are_dropped() {
        let recent = RecentEvents::default();
        let subscriber = tracing_subscriber::registry().with(recent.clone());
        let _default = subscriber.set_default();

        tracing::event!(
            target: "bridge",
            Level::INFO,
            { log.target = "x", log.line = 12u32, keep = "yes" },
            "hello"
        );

        let events = recent.snapshot();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].target, "bridge");
        assert_eq!(events[0].message, "hello keep=yes");
    }
}
