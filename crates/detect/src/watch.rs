//! The worker every strategy runs inside: one thread owning the platform
//! source, the mailbox its callbacks post to, and the publisher that turns
//! a reading into an event. The strategy supplies the platform half.
//!
//! Exactly one strategy compiles for a target, so there is no strategy
//! trait: a trait with a single implementation would hide nothing. The
//! contract is the module aliased as `strategy` below, four items long
//! (`NAME`, `StartError`, `Source::start`, `Source::refresh`), and a module
//! that does not meet it fails that target's build.

use std::error::Error;
use std::io;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime};

use ryuuji_core::PlaybackEvent;
use tracing::debug;

use crate::SessionFacts;
use crate::session::{Dedup, Reading, Tracking, observe};

#[cfg(target_os = "linux")]
use crate::mpris as strategy;
#[cfg(windows)]
use crate::smtc as strategy;

const STARTUP_TIMEOUT: Duration = Duration::from_secs(5);
/// A player that never raises a timeline event still moves its position,
/// and a session that was unreadable may be ready now.
const POLL: Duration = Duration::from_secs(1);

type Sink = Box<dyn Fn(PlaybackEvent) + Send>;
type StartResult = Result<(), Box<dyn Error + Send + Sync>>;

#[derive(Debug, thiserror::Error)]
pub enum WatchError {
    /// The platform's own error is boxed, so the chain is still walkable
    /// while the crate's public surface names no platform type.
    #[error("the {strategy} strategy could not start")]
    Start {
        strategy: &'static str,
        #[source]
        source: Box<dyn Error + Send + Sync>,
    },
    #[error("the {0} worker did not start within {1:?}")]
    StartupTimeout(&'static str, Duration),
    #[error("could not spawn the {0} worker thread")]
    Thread(&'static str, #[source] io::Error),
    #[error("the {0} worker exited before reporting")]
    Exited(&'static str),
}

/// Runs until dropped. `sink` is called on the worker thread, never on a
/// platform callback thread.
pub fn watch(sink: impl Fn(PlaybackEvent) + Send + 'static) -> Result<Watcher, WatchError> {
    let (tx, rx) = mpsc::channel();
    let (ready_tx, ready_rx) = mpsc::sync_channel(1);
    let mirror = Mirror::default();
    let spawn = Spawn {
        waker: Waker(tx.clone()),
        rx,
        ready: ready_tx,
        mirror: mirror.clone(),
        sink: Box::new(sink),
    };
    let thread = thread::Builder::new()
        .name(format!("ryuuji-{}", strategy::NAME.to_lowercase()))
        .spawn(move || spawn.run())
        .map_err(|err| WatchError::Thread(strategy::NAME, err))?;
    match ready_rx.recv_timeout(STARTUP_TIMEOUT) {
        Ok(Ok(())) => Ok(Watcher {
            tx,
            thread: Some(thread),
            mirror,
        }),
        Ok(Err(source)) => {
            let _ = thread.join();
            Err(WatchError::Start {
                strategy: strategy::NAME,
                source,
            })
        }
        Err(RecvTimeoutError::Timeout) => {
            Err(WatchError::StartupTimeout(strategy::NAME, STARTUP_TIMEOUT))
        }
        Err(RecvTimeoutError::Disconnected) => {
            let _ = thread.join();
            Err(WatchError::Exited(strategy::NAME))
        }
    }
}

pub struct Watcher {
    tx: Sender<Msg>,
    thread: Option<JoinHandle<()>>,
    mirror: Mirror,
}

impl Watcher {
    /// Every session seen at the last refresh, copied out under the lock.
    pub fn sessions(&self) -> Vec<SessionFacts> {
        self.mirror.read()
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

/// The rows Diagnostics reads. One writer; a poisoned lock is taken
/// anyway, because the rows are a display copy nothing decides on.
#[derive(Clone, Default)]
struct Mirror(Arc<Mutex<Vec<SessionFacts>>>);

impl Mirror {
    fn publish(&self, rows: Vec<SessionFacts>) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = rows;
    }

    fn read(&self) -> Vec<SessionFacts> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

enum Msg {
    SessionsChanged,
    SessionChanged,
    Stop,
}

/// What a platform callback holds. Posting is all a callback ever does, so
/// no callback thread touches the table, the dedup or the sink. `Stop` is
/// not reachable from here: only [`Watcher`]'s drop sends it.
#[derive(Clone)]
pub(crate) struct Waker(Sender<Msg>);

impl Waker {
    /// The set of sessions may have changed.
    pub(crate) fn sessions_changed(&self) {
        let _ = self.0.send(Msg::SessionsChanged);
    }

    /// One session's properties changed.
    pub(crate) fn session_changed(&self) {
        let _ = self.0.send(Msg::SessionChanged);
    }
}

/// What woke the worker, after a burst has been coalesced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Wake {
    /// The platform announced a change to the session set, so per-session
    /// subscriptions may be attached to sessions that are gone.
    pub sessions_changed: bool,
}

/// What one refresh produced. A strategy that could not read the platform
/// returns `Blind` rather than an empty reading: an empty reading means
/// every session vanished, and would synthesise a Stopped.
pub(crate) enum Refresh<'a> {
    Read(Reading<'a>),
    Blind,
}

/// How soon the worker looks again when the platform says nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pace {
    Poll(Duration),
    /// Nothing is moving; wait for the platform to speak.
    Idle,
}

impl Pace {
    fn watching(playing: bool, unreadable: usize) -> Pace {
        if playing || unreadable > 0 {
            Pace::Poll(POLL)
        } else {
            Pace::Idle
        }
    }
}

struct Mailbox {
    rx: Receiver<Msg>,
}

impl Mailbox {
    /// Blocks until the next refresh is due, coalescing a burst into one
    /// [`Wake`] without losing the session-set signal. `None` means stop.
    fn next(&mut self, pace: Pace) -> Option<Wake> {
        let mut wake = Wake {
            sessions_changed: false,
        };
        let first = match pace {
            Pace::Poll(after) => match self.rx.recv_timeout(after) {
                Ok(msg) => msg,
                Err(RecvTimeoutError::Timeout) => return Some(wake),
                Err(RecvTimeoutError::Disconnected) => return None,
            },
            Pace::Idle => self.rx.recv().ok()?,
        };
        if !wake.note(first) {
            return None;
        }
        while let Ok(msg) = self.rx.try_recv() {
            if !wake.note(msg) {
                return None;
            }
        }
        Some(wake)
    }
}

impl Wake {
    /// Records what one message means for the next refresh. False means
    /// stop.
    fn note(&mut self, msg: Msg) -> bool {
        match msg {
            Msg::SessionsChanged => {
                self.sessions_changed = true;
                true
            }
            Msg::SessionChanged => true,
            Msg::Stop => false,
        }
    }
}

/// The one path to core: every session becomes a Diagnostics row, the
/// watched ones become at most one event. The counts `observe` needs are
/// derived from the rows rather than carried beside them, so a strategy
/// cannot report a matched session it forgot to count.
struct Publisher {
    dedup: Dedup,
    mirror: Mirror,
    sink: Sink,
}

impl Publisher {
    fn publish(&mut self, reading: Reading<'_>, now: SystemTime) -> Pace {
        let mut rows = Vec::with_capacity(reading.sessions.len());
        let mut watched = Vec::new();
        let mut unreadable = 0;
        for seen in reading.sessions {
            rows.push(seen.facts());
            match seen.tracking {
                Tracking::Unknown => {}
                Tracking::Unreadable(_) => unreadable += 1,
                Tracking::Watched(matched) => watched.push(matched),
            }
        }
        if watched.len() > 1 {
            debug!(
                count = watched.len(),
                "several sessions match the player table"
            );
        }
        self.mirror.publish(rows);
        let observation = observe(&watched, unreadable, now, &reading.front);
        if let Some(event) = self.dedup.admit(observation, now) {
            debug!(
                player = event.player,
                title = event.title,
                status = event.status.label(),
                position_ms = event.position.as_millis(),
                duration_ms = event.duration.as_millis(),
                foreground = ?event.foreground,
                "playback event"
            );
            (self.sink)(event);
        }
        Pace::watching(self.dedup.is_playing(), unreadable)
    }
}

/// What the worker thread starts from.
struct Spawn {
    waker: Waker,
    rx: Receiver<Msg>,
    ready: SyncSender<StartResult>,
    mirror: Mirror,
    sink: Sink,
}

impl Spawn {
    fn run(self) {
        let mut source = match strategy::Source::start(self.waker) {
            Ok(source) => source,
            Err(err) => {
                let _ = self.ready.send(Err(Box::new(err)));
                return;
            }
        };
        if self.ready.send(Ok(())).is_err() {
            return;
        }
        let mut publisher = Publisher {
            dedup: Dedup::default(),
            mirror: self.mirror,
            sink: self.sink,
        };
        let mut mailbox = Mailbox { rx: self.rx };
        let mut wake = Wake {
            sessions_changed: true,
        };
        let mut pace = Pace::Idle;
        loop {
            let now = SystemTime::now();
            match source.refresh(wake, now) {
                Refresh::Read(reading) => pace = publisher.publish(reading, now),
                // The strategy has logged why. The pace stands, so a failed
                // read while something plays keeps polling.
                Refresh::Blind => {}
            }
            match mailbox.next(pace) {
                Some(next) => wake = next,
                None => break,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::channel;

    use ryuuji_core::PlaybackStatus;

    use super::*;
    use crate::players::Player;
    use crate::session::{Front, Matched, Seen, SessionSnapshot, SessionState};

    fn new_mailbox() -> (Sender<Msg>, Mailbox) {
        let (tx, rx) = channel();
        (tx, Mailbox { rx })
    }

    fn player(name: &str) -> Player {
        Player {
            name: name.to_owned(),
            smtc_app_ids: Vec::new(),
            mpris_ids: Vec::new(),
            executables: Vec::new(),
        }
    }

    fn snapshot(state: SessionState) -> SessionSnapshot {
        SessionSnapshot {
            title: "Episode 3".to_owned(),
            state,
            start: Duration::ZERO,
            end: Duration::from_secs(1440),
            position: Duration::from_secs(90),
            updated: None,
        }
    }

    fn now() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000)
    }

    fn publisher() -> (Publisher, Receiver<PlaybackEvent>) {
        let (tx, rx) = channel();
        let publisher = Publisher {
            dedup: Dedup::default(),
            mirror: Mirror::default(),
            sink: Box::new(move |event| {
                let _ = tx.send(event);
            }),
        };
        (publisher, rx)
    }

    #[test]
    fn a_burst_coalesces_into_one_wake_that_keeps_the_session_set_bit() {
        let (tx, mut mailbox) = new_mailbox();
        tx.send(Msg::SessionChanged).unwrap();
        tx.send(Msg::SessionsChanged).unwrap();
        tx.send(Msg::SessionChanged).unwrap();
        assert_eq!(
            mailbox.next(Pace::Idle),
            Some(Wake {
                sessions_changed: true
            })
        );
        tx.send(Msg::SessionChanged).unwrap();
        assert_eq!(
            mailbox.next(Pace::Idle),
            Some(Wake {
                sessions_changed: false
            })
        );
    }

    #[test]
    fn a_poll_that_hears_nothing_wakes_without_the_set_bit() {
        let (_tx, mut mailbox) = new_mailbox();
        assert_eq!(
            mailbox.next(Pace::Poll(Duration::from_millis(5))),
            Some(Wake {
                sessions_changed: false
            })
        );
    }

    #[test]
    fn stop_ends_the_worker_even_in_the_middle_of_a_burst() {
        let (tx, mut mailbox) = new_mailbox();
        tx.send(Msg::SessionsChanged).unwrap();
        tx.send(Msg::Stop).unwrap();
        tx.send(Msg::SessionChanged).unwrap();
        assert_eq!(mailbox.next(Pace::Idle), None);
    }

    #[test]
    fn a_dropped_sender_ends_the_worker() {
        let (tx, mut mailbox) = new_mailbox();
        drop(tx);
        assert_eq!(mailbox.next(Pace::Idle), None);
        let (tx, mut mailbox) = new_mailbox();
        drop(tx);
        assert_eq!(mailbox.next(Pace::Poll(Duration::from_millis(5))), None);
    }

    #[test]
    fn the_worker_polls_while_playing_or_while_a_session_is_unreadable() {
        assert_eq!(Pace::watching(true, 0), Pace::Poll(POLL));
        assert_eq!(Pace::watching(false, 1), Pace::Poll(POLL));
        assert_eq!(Pace::watching(false, 0), Pace::Idle);
    }

    #[test]
    fn every_session_reaches_the_mirror_and_only_the_watched_one_reaches_the_sink() {
        let mpv = player("mpv");
        let (mut publisher, events) = publisher();
        let reading = Reading {
            front: Front::Unknown,
            sessions: vec![
                Seen {
                    app_id: "Spotify.exe".to_owned(),
                    status: "Playing".to_owned(),
                    tracking: Tracking::Unknown,
                },
                Seen {
                    app_id: "mpv.exe".to_owned(),
                    status: "Playing".to_owned(),
                    tracking: Tracking::Watched(Matched {
                        player: &mpv,
                        snapshot: snapshot(SessionState::Playing),
                    }),
                },
            ],
        };
        let pace = publisher.publish(reading, now());
        assert_eq!(pace, Pace::Poll(POLL));
        let rows = publisher.mirror.read();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].player, None);
        assert_eq!(rows[1].player.as_deref(), Some("mpv"));
        assert_eq!(rows[1].title, "Episode 3");
        let event = events.try_recv().unwrap();
        assert_eq!(event.player, "mpv");
        assert_eq!(event.status, PlaybackStatus::Playing);
        assert!(events.try_recv().is_err());
    }

    #[test]
    fn an_unreadable_session_is_counted_so_the_observation_is_not_absent() {
        let mpv = player("mpv");
        let (mut publisher, events) = publisher();
        let watched = Reading {
            front: Front::Unknown,
            sessions: vec![Seen {
                app_id: "mpv.exe".to_owned(),
                status: "Playing".to_owned(),
                tracking: Tracking::Watched(Matched {
                    player: &mpv,
                    snapshot: snapshot(SessionState::Playing),
                }),
            }],
        };
        publisher.publish(watched, now());
        events.try_recv().unwrap();
        let unreadable = Reading {
            front: Front::Unknown,
            sessions: vec![Seen {
                app_id: "mpv.exe".to_owned(),
                status: "Playing".to_owned(),
                tracking: Tracking::Unreadable(&mpv),
            }],
        };
        let pace = publisher.publish(unreadable, now() + Duration::from_secs(1));
        assert_eq!(pace, Pace::Poll(POLL));
        assert!(events.try_recv().is_err(), "no Stopped was synthesised");
        assert_eq!(publisher.mirror.read()[0].player.as_deref(), Some("mpv"));
    }

    #[test]
    fn a_repeated_reading_is_not_sunk_twice() {
        let mpv = player("mpv");
        let (mut publisher, events) = publisher();
        for offset in [0, 1] {
            let reading = Reading {
                front: Front::Unknown,
                sessions: vec![Seen {
                    app_id: "mpv.exe".to_owned(),
                    status: "Paused".to_owned(),
                    tracking: Tracking::Watched(Matched {
                        player: &mpv,
                        snapshot: snapshot(SessionState::Paused),
                    }),
                }],
            };
            let pace = publisher.publish(reading, now() + Duration::from_secs(offset));
            assert_eq!(pace, Pace::Idle);
        }
        assert_eq!(events.try_iter().count(), 1);
    }

    #[test]
    fn an_empty_reading_after_a_watched_one_synthesises_stopped() {
        let mpv = player("mpv");
        let (mut publisher, events) = publisher();
        publisher.publish(
            Reading {
                front: Front::Unknown,
                sessions: vec![Seen {
                    app_id: "mpv.exe".to_owned(),
                    status: "Playing".to_owned(),
                    tracking: Tracking::Watched(Matched {
                        player: &mpv,
                        snapshot: snapshot(SessionState::Playing),
                    }),
                }],
            },
            now(),
        );
        events.try_recv().unwrap();
        let pace = publisher.publish(
            Reading {
                front: Front::Unknown,
                sessions: Vec::new(),
            },
            now() + Duration::from_secs(1),
        );
        assert_eq!(pace, Pace::Idle);
        assert_eq!(events.try_recv().unwrap().status, PlaybackStatus::Stopped);
        assert!(publisher.mirror.read().is_empty());
    }
}
