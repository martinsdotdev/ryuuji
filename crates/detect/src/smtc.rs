//! The Windows SMTC strategy. One worker thread owns the session manager,
//! the event subscriptions and the dedup state; WinRT callbacks only post a
//! message to it, so the sink never runs on a WinRT thread.

use std::io;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime};

use ryuuji_core::PlaybackEvent;
use tracing::{debug, warn};
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSession as Session,
    GlobalSystemMediaTransportControlsSessionManager as Manager,
};
use windows::core::EventRevoker;

use crate::session::{Dedup, Matched, Observation, RawStatus, SessionSnapshot, choose, normalize};
use crate::{PlayerTable, SessionFacts};

const STARTUP_TIMEOUT: Duration = Duration::from_secs(5);
/// While a session plays, the timeline is re-read on this cadence even if
/// the player never raises a timeline event.
const PLAYING_POLL: Duration = Duration::from_secs(1);

pub(crate) enum Msg {
    SessionsChanged,
    SessionChanged,
    Stop,
}

#[derive(Debug, thiserror::Error)]
pub enum WatchError {
    #[error("SMTC session manager unavailable")]
    Manager(#[source] windows::core::Error),
    #[error("SMTC worker did not start within {0:?}")]
    StartupTimeout(Duration),
    #[error("could not spawn the SMTC worker thread")]
    Thread(#[source] io::Error),
}

type Sink = Box<dyn Fn(PlaybackEvent) + Send>;

/// Runs until dropped. `sink` is called on the worker thread, never on a
/// WinRT callback thread.
pub fn watch(sink: impl Fn(PlaybackEvent) + Send + 'static) -> Result<Watcher, WatchError> {
    let (tx, rx) = mpsc::channel();
    let (ready_tx, ready_rx) = mpsc::sync_channel(1);
    let facts = Arc::new(Mutex::new(Vec::new()));
    let worker = Spawn {
        tx: tx.clone(),
        rx,
        ready: ready_tx,
        facts: Arc::clone(&facts),
        sink: Box::new(sink),
    };
    let thread = thread::Builder::new()
        .name("ryuuji-smtc".to_owned())
        .spawn(move || worker.run())
        .map_err(WatchError::Thread)?;
    match ready_rx.recv_timeout(STARTUP_TIMEOUT) {
        Ok(Ok(())) => Ok(Watcher {
            tx,
            thread: Some(thread),
            facts,
        }),
        Ok(Err(err)) => {
            warn!(error = %err, "smtc manager unavailable");
            let _ = thread.join();
            Err(WatchError::Manager(err))
        }
        Err(RecvTimeoutError::Timeout) => Err(WatchError::StartupTimeout(STARTUP_TIMEOUT)),
        Err(RecvTimeoutError::Disconnected) => {
            let _ = thread.join();
            Err(WatchError::Thread(io::Error::other(
                "the SMTC worker exited before reporting",
            )))
        }
    }
}

pub struct Watcher {
    tx: Sender<Msg>,
    thread: Option<JoinHandle<()>>,
    facts: Arc<Mutex<Vec<SessionFacts>>>,
}

impl Watcher {
    /// Every session seen at the last refresh, copied out under the lock.
    pub fn sessions(&self) -> Vec<SessionFacts> {
        self.facts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        let _ = self.tx.send(Msg::Stop);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// What the worker thread starts from.
struct Spawn {
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    ready: SyncSender<windows::core::Result<()>>,
    facts: Arc<Mutex<Vec<SessionFacts>>>,
    sink: Sink,
}

impl Spawn {
    fn run(self) {
        let manager = windows::core::init_mta()
            .and_then(|()| Manager::RequestAsync()?.join())
            .and_then(|manager| {
                let tx = self.tx.clone();
                let revoker = manager.SessionsChanged(move |_, _| {
                    let _ = tx.send(Msg::SessionsChanged);
                })?;
                Ok((manager, revoker))
            });
        let (manager, manager_revoker) = match manager {
            Ok(started) => started,
            Err(err) => {
                let _ = self.ready.send(Err(err));
                return;
            }
        };
        if self.ready.send(Ok(())).is_err() {
            return;
        }
        let mut worker = Worker {
            manager,
            table: PlayerTable::builtin(),
            tx: self.tx,
            sessions: Vec::new(),
            dedup: Dedup::default(),
            facts: self.facts,
            sink: self.sink,
        };
        worker.refresh_and_log();
        while worker.wait(&self.rx) {
            worker.refresh_and_log();
        }
        worker.sessions.clear();
        drop(manager_revoker);
    }
}

struct Worker {
    manager: Manager,
    table: PlayerTable,
    tx: Sender<Msg>,
    sessions: Vec<Subscribed>,
    dedup: Dedup,
    facts: Arc<Mutex<Vec<SessionFacts>>>,
    sink: Sink,
}

/// A matched session whose events feed the worker; dropping it revokes them.
struct Subscribed {
    app_id: String,
    _revokers: [EventRevoker; 3],
}

impl Worker {
    /// Blocks until the next refresh is due. False means stop.
    fn wait(&self, rx: &Receiver<Msg>) -> bool {
        let first = if self.dedup.is_playing() {
            match rx.recv_timeout(PLAYING_POLL) {
                Ok(msg) => msg,
                Err(RecvTimeoutError::Timeout) => return true,
                Err(RecvTimeoutError::Disconnected) => return false,
            }
        } else {
            match rx.recv() {
                Ok(msg) => msg,
                Err(_) => return false,
            }
        };
        if matches!(first, Msg::Stop) {
            return false;
        }
        while let Ok(msg) = rx.try_recv() {
            if matches!(msg, Msg::Stop) {
                return false;
            }
        }
        true
    }

    fn refresh_and_log(&mut self) {
        if let Err(err) = self.refresh(SystemTime::now()) {
            warn!(error = %err, "smtc refresh failed");
        }
    }

    fn refresh(&mut self, now: SystemTime) -> windows::core::Result<()> {
        let mut facts = Vec::new();
        let mut matched = Vec::new();
        let mut live = Vec::new();
        for session in &self.manager.GetSessions()? {
            let app_id = session.SourceAppUserModelId()?.to_string_lossy();
            let raw = session.GetPlaybackInfo()?.PlaybackStatus()?.0;
            let status = RawStatus::from_winrt(raw);
            let status_label =
                status.map_or_else(|| format!("Unknown({raw})"), |s| s.label().to_owned());
            debug!(app_id, status = status_label, "smtc session");
            let player = self.table.match_smtc(&app_id);
            let mut title = String::new();
            if let (Some(player), Some(status)) = (player, status) {
                title = session
                    .TryGetMediaPropertiesAsync()?
                    .join()?
                    .Title()?
                    .to_string_lossy();
                let timeline = session.GetTimelineProperties()?;
                let snapshot = SessionSnapshot {
                    app_id: app_id.clone(),
                    title: title.clone(),
                    status,
                    start: span(timeline.StartTime()?),
                    end: span(timeline.EndTime()?),
                    position: span(timeline.Position()?),
                };
                debug!(
                    app_id,
                    title,
                    status = status.label(),
                    player = player.name,
                    position_ms = snapshot.position.as_millis(),
                    start_ms = snapshot.start.as_millis(),
                    end_ms = snapshot.end.as_millis(),
                    "smtc matched session"
                );
                matched.push(Matched { player, snapshot });
                live.push((app_id.clone(), session));
            }
            facts.push(SessionFacts {
                app_id,
                title,
                status: status_label,
                player: player.map(|p| p.name.clone()),
            });
        }
        if matched.len() > 1 {
            debug!(
                count = matched.len(),
                "several smtc sessions match the player table"
            );
        }
        let observation = match choose(&matched) {
            None => Observation::Absent,
            Some(chosen) => {
                normalize(chosen, now).map_or(Observation::Transitional, Observation::Seen)
            }
        };
        self.resubscribe(live)?;
        *self.facts.lock().unwrap_or_else(PoisonError::into_inner) = facts;
        if let Some(event) = self.dedup.admit(observation, now) {
            debug!(
                player = event.player,
                title = event.title,
                status = event.status.label(),
                position_ms = event.position.as_millis(),
                duration_ms = event.duration.as_millis(),
                "playback event"
            );
            (self.sink)(event);
        }
        Ok(())
    }

    /// Re-registers session events only when the set of matched app ids
    /// changed, dropping the stale registrations first.
    fn resubscribe(&mut self, live: Vec<(String, Session)>) -> windows::core::Result<()> {
        let unchanged = self.sessions.len() == live.len()
            && self
                .sessions
                .iter()
                .zip(&live)
                .all(|(subscribed, (app_id, _))| subscribed.app_id == *app_id);
        if unchanged {
            return Ok(());
        }
        self.sessions.clear();
        for (app_id, session) in live {
            let media = self.tx.clone();
            let playback = self.tx.clone();
            let timeline = self.tx.clone();
            let revokers = [
                session.MediaPropertiesChanged(move |_, _| {
                    let _ = media.send(Msg::SessionChanged);
                })?,
                session.PlaybackInfoChanged(move |_, _| {
                    let _ = playback.send(Msg::SessionChanged);
                })?,
                session.TimelinePropertiesChanged(move |_, _| {
                    let _ = timeline.send(Msg::SessionChanged);
                })?,
            ];
            debug!(app_id, "subscribed to smtc session events");
            self.sessions.push(Subscribed {
                app_id,
                _revokers: revokers,
            });
        }
        Ok(())
    }
}

/// A WinRT `TimeSpan` as a `Duration`; negative spans read as zero.
fn span(value: impl TryInto<Duration>) -> Duration {
    value.try_into().unwrap_or_default()
}
