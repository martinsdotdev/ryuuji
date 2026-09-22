//! The Windows SMTC strategy: the WinRT session manager, its event
//! subscriptions, and the reads that turn its sessions into a
//! [`Reading`]. Everything else about the worker lives in [`crate::watch`].
//! WinRT callbacks only post to the [`Waker`], so no read happens on a
//! WinRT thread.

use std::collections::HashSet;
use std::time::{Duration, SystemTime};

use ryuuji_core::PlayerRow;
use tracing::{debug, warn};
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSession as Session,
    GlobalSystemMediaTransportControlsSessionManager as Manager,
};
use windows::core::EventRevoker;

use crate::players::PlayerTable;
use crate::session::{Matched, Reading, Seen, SessionSnapshot, SessionState, Tracking};
use crate::watch::{Refresh, Wake, Waker};

mod foreground;
mod gecko;

pub(crate) const NAME: &str = "SMTC";

#[derive(Debug, thiserror::Error)]
pub(crate) enum StartError {
    #[error("SMTC session manager unavailable")]
    Manager(#[source] windows::core::Error),
}

/// `GlobalSystemMediaTransportControlsSessionPlaybackStatus` as it comes off
/// the wire, in declaration order. Parsed into [`SessionState`] here; no
/// other module sees six values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RawStatus {
    Closed,
    Opened,
    Changing,
    Stopped,
    Playing,
    Paused,
}

impl RawStatus {
    const ALL: [RawStatus; 6] = [
        RawStatus::Closed,
        RawStatus::Opened,
        RawStatus::Changing,
        RawStatus::Stopped,
        RawStatus::Playing,
        RawStatus::Paused,
    ];

    fn from_winrt(value: i32) -> Option<RawStatus> {
        usize::try_from(value)
            .ok()
            .and_then(|index| RawStatus::ALL.get(index).copied())
    }

    fn label(self) -> &'static str {
        match self {
            RawStatus::Closed => "Closed",
            RawStatus::Opened => "Opened",
            RawStatus::Changing => "Changing",
            RawStatus::Stopped => "Stopped",
            RawStatus::Playing => "Playing",
            RawStatus::Paused => "Paused",
        }
    }

    /// Closed and Stopped are Stopped; Opened and Changing are a session
    /// that has not settled.
    fn state(self) -> SessionState {
        match self {
            RawStatus::Closed | RawStatus::Stopped => SessionState::Stopped,
            RawStatus::Playing => SessionState::Playing,
            RawStatus::Paused => SessionState::Paused,
            RawStatus::Opened | RawStatus::Changing => SessionState::Settling,
        }
    }
}

pub(crate) struct Source {
    manager: Manager,
    /// Dropped with the source, which revokes the session-set event.
    _manager_revoker: EventRevoker,
    table: PlayerTable,
    waker: Waker,
    /// One revoker triple per subscribed session; dropping one revokes its
    /// events.
    subscriptions: Vec<[EventRevoker; 3]>,
    /// The manager announced a change to the session set, so the current
    /// subscriptions may be attached to sessions that are gone. Held until
    /// a resubscribe succeeds, so a failed one is retried.
    stale: bool,
    /// Hash-shaped app ids still unmatched after a discovery, so one
    /// unknown browser does not scan the registry on every refresh.
    unknown_hashes: HashSet<String>,
}

impl Source {
    pub(crate) fn start(waker: Waker, table: PlayerTable) -> Result<Source, StartError> {
        let (manager, revoker) = windows::core::init_mta()
            .and_then(|()| Manager::RequestAsync()?.join())
            .and_then(|manager| {
                let waker = waker.clone();
                let revoker = manager.SessionsChanged(move |_, _| waker.sessions_changed())?;
                Ok((manager, revoker))
            })
            .map_err(StartError::Manager)?;
        Ok(Source {
            manager,
            _manager_revoker: revoker,
            table,
            waker,
            subscriptions: Vec::new(),
            stale: true,
            unknown_hashes: HashSet::new(),
        })
    }

    /// One pass over every session. Browsers are discovered before any
    /// matching when an unmatched session shows an install hash the table
    /// has not seen, so a Gecko browser matches on the refresh that first
    /// shows it. Discovery is not done at start: it walks the registry and
    /// resolves every registered browser's install path, and a path on an
    /// unreachable share would stall the startup handshake.
    ///
    /// `now` is unused here: the timeline carries its own write time, which
    /// the session logic reads against the publisher's clock.
    pub(crate) fn refresh(&mut self, wake: Wake, _now: SystemTime) -> Refresh<'_> {
        self.stale |= wake.sessions_changed;
        let sessions = match self.manager.GetSessions() {
            Ok(sessions) => sessions,
            Err(err) => {
                warn!(error = %err, "smtc sessions unavailable");
                return Refresh::Blind;
            }
        };
        let mut read = Vec::new();
        for session in &sessions {
            match read_session(&session) {
                Ok((app_id, raw)) => read.push((session, app_id, raw)),
                Err(err) => debug!(error = %err, "smtc session not ready"),
            }
        }
        if read
            .iter()
            .any(|(_, app_id, _)| self.is_unknown_hash(app_id))
        {
            let added = discover_browsers(&mut self.table);
            for (_, app_id, _) in &read {
                if self.is_unknown_hash(app_id) {
                    self.unknown_hashes.insert(app_id.clone());
                }
            }
            debug!(added, "rescanned browsers for an unknown install hash");
        }

        let front = foreground::front();
        let mut seen = Vec::with_capacity(read.len());
        let mut live = Vec::new();
        for (session, app_id, raw) in read {
            let status = RawStatus::from_winrt(raw);
            let status_label =
                status.map_or_else(|| format!("Unknown({raw})"), |s| s.label().to_owned());
            debug!(app_id, status = status_label, "smtc session");
            let tracking = match (self.table.match_app_id(&app_id), status) {
                (Some(player), _) if player.hidden => Tracking::Hidden(player),
                (Some(player), Some(status)) => {
                    live.push(session.clone());
                    match read_snapshot(&session, status) {
                        Ok(snapshot) => {
                            debug!(
                                app_id,
                                title = snapshot.title,
                                status = status.label(),
                                player = player.name,
                                position_ms = snapshot.position.as_millis(),
                                start_ms = snapshot.start.as_millis(),
                                end_ms = snapshot.end.as_millis(),
                                "smtc matched session"
                            );
                            Tracking::Watched(Matched { player, snapshot })
                        }
                        Err(err) => {
                            debug!(app_id, error = %err, "smtc session not ready");
                            Tracking::Unreadable(player)
                        }
                    }
                }
                _ => Tracking::Unknown,
            };
            seen.push(Seen {
                app_id,
                status: status_label,
                tracking,
            });
        }
        if self.stale || live.len() != self.subscriptions.len() {
            match resubscribe(&mut self.subscriptions, &self.waker, live) {
                Ok(()) => self.stale = false,
                Err(err) => warn!(error = %err, "smtc resubscribe failed"),
            }
        }
        Refresh::Read(Reading {
            front,
            sessions: seen,
        })
    }

    /// Lays the person's new rows over the same base, so every browser
    /// discovery found stays found. When they changed, every session is
    /// subscribed afresh, since which ones are watched may have. The hashes
    /// discovery could not tie stay untied, because the base discovery reads
    /// does not depend on the rows.
    pub(crate) fn set_rows(&mut self, rows: Vec<PlayerRow>) {
        if self.table.set_rows(rows) {
            self.stale = true;
        }
    }

    fn is_unknown_hash(&self, app_id: &str) -> bool {
        gecko::is_install_hash(app_id)
            && !self.unknown_hashes.contains(app_id)
            && self.table.match_app_id(app_id).is_none()
    }
}

/// Adds the browsers registered on this machine to the table. Returns how
/// many rows changed.
fn discover_browsers(table: &mut PlayerTable) -> usize {
    let found = gecko::discover();
    for player in &found {
        debug!(
            name = player.name,
            app_id = player.smtc_app_ids[0],
            "discovered browser"
        );
    }
    match table.extend(found) {
        Ok(added) => added,
        Err(err) => {
            warn!(error = %err, "discovered browsers rejected");
            0
        }
    }
}

/// Registers the events of every live matched session and drops the
/// previous registrations. A failure part way leaves the previous
/// registrations in place, so the next refresh retries from a known set.
fn resubscribe(
    subscriptions: &mut Vec<[EventRevoker; 3]>,
    waker: &Waker,
    live: Vec<Session>,
) -> windows::core::Result<()> {
    let mut fresh = Vec::with_capacity(live.len());
    for session in live {
        let (media, playback, timeline) = (waker.clone(), waker.clone(), waker.clone());
        fresh.push([
            session.MediaPropertiesChanged(move |_, _| media.session_changed())?,
            session.PlaybackInfoChanged(move |_, _| playback.session_changed())?,
            session.TimelinePropertiesChanged(move |_, _| timeline.session_changed())?,
        ]);
    }
    *subscriptions = fresh;
    debug!(
        count = subscriptions.len(),
        "subscribed to smtc session events"
    );
    Ok(())
}

/// App id and raw playback status, the reads every session gets.
fn read_session(session: &Session) -> windows::core::Result<(String, i32)> {
    let app_id = session.SourceAppUserModelId()?.to_string_lossy();
    let raw = session.GetPlaybackInfo()?.PlaybackStatus()?.0;
    Ok((app_id, raw))
}

/// Media properties and timeline, read only for matched sessions.
fn read_snapshot(session: &Session, status: RawStatus) -> windows::core::Result<SessionSnapshot> {
    let title = session
        .TryGetMediaPropertiesAsync()?
        .join()?
        .Title()?
        .to_string_lossy();
    let timeline = session.GetTimelineProperties()?;
    Ok(SessionSnapshot {
        title,
        state: status.state(),
        start: span(timeline.StartTime()?),
        end: span(timeline.EndTime()?),
        position: span(timeline.Position()?),
        updated: instant(timeline.LastUpdatedTime()?),
    })
}

/// A WinRT `TimeSpan` as a `Duration`; negative spans read as zero.
fn span(value: impl TryInto<Duration>) -> Duration {
    value.try_into().unwrap_or_default()
}

/// A WinRT `DateTime` as a `SystemTime`. An unset one is zero, which would
/// otherwise read as a moment in 1601, so nothing at or before the Unix epoch
/// counts as known.
fn instant(value: impl TryInto<SystemTime>) -> Option<SystemTime> {
    value
        .try_into()
        .ok()
        .filter(|time| *time > SystemTime::UNIX_EPOCH)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_status_round_trips_winrt_values() {
        for (value, status) in RawStatus::ALL.into_iter().enumerate() {
            assert_eq!(RawStatus::from_winrt(value as i32), Some(status));
        }
        assert_eq!(RawStatus::from_winrt(6), None);
        assert_eq!(RawStatus::from_winrt(-1), None);
        assert_eq!(RawStatus::Closed.label(), "Closed");
        assert_eq!(RawStatus::Paused.label(), "Paused");
    }

    #[test]
    fn closed_and_stopped_settle_as_stopped_while_opened_and_changing_do_not() {
        assert_eq!(RawStatus::Closed.state(), SessionState::Stopped);
        assert_eq!(RawStatus::Stopped.state(), SessionState::Stopped);
        assert_eq!(RawStatus::Playing.state(), SessionState::Playing);
        assert_eq!(RawStatus::Paused.state(), SessionState::Paused);
        assert_eq!(RawStatus::Opened.state(), SessionState::Settling);
        assert_eq!(RawStatus::Changing.state(), SessionState::Settling);
    }
}
