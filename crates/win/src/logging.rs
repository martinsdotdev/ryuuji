use std::path::Path;

use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

/// Installs the global subscriber: a daily-rolling file in `logs`, plus
/// stderr in debug builds. Keep the returned guard alive for the life of the
/// process; dropping it flushes and stops the file writer.
pub fn init(logs: &Path) -> WorkerGuard {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let (writer, guard) =
        tracing_appender::non_blocking(tracing_appender::rolling::daily(logs, "ryuuji.log"));
    let file = fmt::layer().with_ansi(false).with_writer(writer);
    let registry = tracing_subscriber::registry().with(filter).with(file);

    #[cfg(debug_assertions)]
    let registry = registry.with(fmt::layer().with_writer(std::io::stderr));

    registry.init();
    guard
}
