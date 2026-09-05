//! The pure half of the SMTC strategy: raw session facts in, at most one
//! normalized [`PlaybackEvent`] out. Nothing here touches WinRT, so it is
//! tested on every platform.

use std::time::{Duration, SystemTime};

use ryuuji_core::{PlaybackEvent, PlaybackSource, PlaybackStatus};

use crate::players::Player;

/// `GlobalSystemMediaTransportControlsSessionPlaybackStatus` as it comes off
/// the wire, in declaration order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RawStatus {
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

    pub(crate) fn from_winrt(value: i32) -> Option<RawStatus> {
        usize::try_from(value)
            .ok()
            .and_then(|index| RawStatus::ALL.get(index).copied())
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            RawStatus::Closed => "Closed",
            RawStatus::Opened => "Opened",
            RawStatus::Changing => "Changing",
            RawStatus::Stopped => "Stopped",
            RawStatus::Playing => "Playing",
            RawStatus::Paused => "Paused",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SessionSnapshot {
    pub title: String,
    pub status: RawStatus,
    pub start: Duration,
    pub end: Duration,
    pub position: Duration,
}

pub(crate) struct Matched<'a> {
    pub player: &'a Player,
    pub snapshot: SessionSnapshot,
}

/// Prefer the Playing session, else the first.
pub(crate) fn choose<'a, 'b>(matched: &'b [Matched<'a>]) -> Option<&'b Matched<'a>> {
    matched
        .iter()
        .find(|m| m.snapshot.status == RawStatus::Playing)
        .or_else(|| matched.first())
}

/// Closed and Stopped are Stopped; Opened and Changing are not an
/// observation yet. Times are relative to the timeline start.
pub(crate) fn normalize(matched: &Matched<'_>, now: SystemTime) -> Option<PlaybackEvent> {
    let snapshot = &matched.snapshot;
    let status = match snapshot.status {
        RawStatus::Closed | RawStatus::Stopped => PlaybackStatus::Stopped,
        RawStatus::Playing => PlaybackStatus::Playing,
        RawStatus::Paused => PlaybackStatus::Paused,
        RawStatus::Opened | RawStatus::Changing => return None,
    };
    Some(PlaybackEvent {
        player: matched.player.name.clone(),
        title: snapshot.title.clone(),
        status,
        position: snapshot.position.saturating_sub(snapshot.start),
        duration: snapshot.end.saturating_sub(snapshot.start),
        observed_at: now,
        source: PlaybackSource::Detected,
        foreground: None,
    })
}

pub(crate) enum Observation {
    /// No session matched the player table.
    Absent,
    /// The chosen session is between states and says nothing yet.
    Transitional,
    Seen(PlaybackEvent),
}

/// `unreadable` counts matched sessions whose media or timeline could not be
/// read this refresh. They keep the observation out of `Absent`, since a
/// session that exists but is not ready has not vanished.
pub(crate) fn observe(matched: &[Matched<'_>], unreadable: usize, now: SystemTime) -> Observation {
    match choose(matched) {
        Some(chosen) => normalize(chosen, now).map_or(Observation::Transitional, Observation::Seen),
        None if unreadable > 0 => Observation::Transitional,
        None => Observation::Absent,
    }
}

/// Drops observations equal to the last one in every field except
/// `observed_at`; synthesises Stopped when the session vanishes.
#[derive(Default)]
pub(crate) struct Dedup {
    last: Option<PlaybackEvent>,
}

impl Dedup {
    pub(crate) fn admit(
        &mut self,
        observation: Observation,
        now: SystemTime,
    ) -> Option<PlaybackEvent> {
        let event = match observation {
            Observation::Transitional => return None,
            Observation::Seen(event) => event,
            Observation::Absent => {
                let last = self.last.as_ref()?;
                if last.status == PlaybackStatus::Stopped {
                    return None;
                }
                PlaybackEvent {
                    status: PlaybackStatus::Stopped,
                    observed_at: now,
                    ..last.clone()
                }
            }
        };
        if self
            .last
            .as_ref()
            .is_some_and(|last| same_except_time(&event, last))
        {
            return None;
        }
        self.last = Some(event.clone());
        Some(event)
    }

    pub(crate) fn is_playing(&self) -> bool {
        self.last
            .as_ref()
            .is_some_and(|last| last.status == PlaybackStatus::Playing)
    }
}

fn same_except_time(a: &PlaybackEvent, b: &PlaybackEvent) -> bool {
    let aligned = PlaybackEvent {
        observed_at: b.observed_at,
        ..a.clone()
    };
    aligned == *b
}

#[cfg(test)]
mod tests {
    use super::*;

    fn player(name: &str) -> Player {
        Player {
            name: name.to_owned(),
            smtc_app_ids: Vec::new(),
        }
    }

    fn snapshot(status: RawStatus) -> SessionSnapshot {
        SessionSnapshot {
            title: "Episode 3".to_owned(),
            status,
            start: Duration::ZERO,
            end: Duration::from_secs(1440),
            position: Duration::from_secs(90),
        }
    }

    fn now() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000)
    }

    fn event(status: PlaybackStatus, position_secs: u64) -> PlaybackEvent {
        PlaybackEvent {
            player: "mpv".to_owned(),
            title: "Episode 3".to_owned(),
            status,
            position: Duration::from_secs(position_secs),
            duration: Duration::from_secs(1440),
            observed_at: now(),
            source: PlaybackSource::Detected,
            foreground: None,
        }
    }

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
    fn choose_prefers_the_playing_session() {
        let (mpv, vlc) = (player("mpv"), player("VLC"));
        let matched = [
            Matched {
                player: &mpv,
                snapshot: snapshot(RawStatus::Paused),
            },
            Matched {
                player: &vlc,
                snapshot: snapshot(RawStatus::Playing),
            },
        ];
        assert_eq!(choose(&matched).unwrap().player.name, "VLC");
    }

    #[test]
    fn choose_falls_back_to_the_first() {
        let (mpv, vlc) = (player("mpv"), player("VLC"));
        let matched = [
            Matched {
                player: &mpv,
                snapshot: snapshot(RawStatus::Paused),
            },
            Matched {
                player: &vlc,
                snapshot: snapshot(RawStatus::Stopped),
            },
        ];
        assert_eq!(choose(&matched).unwrap().player.name, "mpv");
        assert!(choose(&[]).is_none());
    }

    #[test]
    fn normalize_maps_closed_and_stopped_to_stopped() {
        let mpv = player("mpv");
        for status in [RawStatus::Closed, RawStatus::Stopped] {
            let matched = Matched {
                player: &mpv,
                snapshot: snapshot(status),
            };
            assert_eq!(
                normalize(&matched, now()),
                Some(event(PlaybackStatus::Stopped, 90))
            );
        }
    }

    #[test]
    fn normalize_maps_playing_and_paused() {
        let mpv = player("mpv");
        for (raw, status) in [
            (RawStatus::Playing, PlaybackStatus::Playing),
            (RawStatus::Paused, PlaybackStatus::Paused),
        ] {
            let matched = Matched {
                player: &mpv,
                snapshot: snapshot(raw),
            };
            assert_eq!(normalize(&matched, now()), Some(event(status, 90)));
        }
    }

    #[test]
    fn normalize_skips_opened_and_changing() {
        let mpv = player("mpv");
        for status in [RawStatus::Opened, RawStatus::Changing] {
            let matched = Matched {
                player: &mpv,
                snapshot: snapshot(status),
            };
            assert_eq!(normalize(&matched, now()), None);
        }
    }

    #[test]
    fn normalize_subtracts_start_time_and_saturates() {
        let mpv = player("mpv");
        let mut shifted = snapshot(RawStatus::Playing);
        shifted.start = Duration::from_secs(100);
        shifted.end = Duration::from_secs(400);
        shifted.position = Duration::from_secs(130);
        let matched = Matched {
            player: &mpv,
            snapshot: shifted.clone(),
        };
        let normalized = normalize(&matched, now()).unwrap();
        assert_eq!(normalized.position, Duration::from_secs(30));
        assert_eq!(normalized.duration, Duration::from_secs(300));

        shifted.position = Duration::from_secs(20);
        shifted.end = Duration::from_secs(50);
        let matched = Matched {
            player: &mpv,
            snapshot: shifted,
        };
        let normalized = normalize(&matched, now()).unwrap();
        assert_eq!(normalized.position, Duration::ZERO);
        assert_eq!(normalized.duration, Duration::ZERO);
    }

    #[test]
    fn unreadable_matched_session_is_transitional_not_absent() {
        assert!(matches!(observe(&[], 1, now()), Observation::Transitional));
        let mpv = player("mpv");
        let matched = [Matched {
            player: &mpv,
            snapshot: snapshot(RawStatus::Playing),
        }];
        assert!(matches!(observe(&matched, 1, now()), Observation::Seen(_)));
    }

    #[test]
    fn no_sessions_is_absent() {
        assert!(matches!(observe(&[], 0, now()), Observation::Absent));
    }

    #[test]
    fn dedup_drops_events_equal_except_observed_at() {
        let mut dedup = Dedup::default();
        let first = event(PlaybackStatus::Playing, 90);
        assert_eq!(
            dedup.admit(Observation::Seen(first.clone()), now()),
            Some(first.clone())
        );
        let later = PlaybackEvent {
            observed_at: now() + Duration::from_secs(1),
            ..first
        };
        assert_eq!(dedup.admit(Observation::Seen(later), now()), None);
        assert!(dedup.is_playing());
    }

    #[test]
    fn dedup_passes_position_changes() {
        let mut dedup = Dedup::default();
        dedup.admit(Observation::Seen(event(PlaybackStatus::Playing, 90)), now());
        let moved = event(PlaybackStatus::Playing, 91);
        assert_eq!(
            dedup.admit(Observation::Seen(moved.clone()), now()),
            Some(moved)
        );
    }

    #[test]
    fn dedup_synthesises_stopped_when_the_session_vanishes() {
        let mut dedup = Dedup::default();
        dedup.admit(Observation::Seen(event(PlaybackStatus::Playing, 90)), now());
        let later = now() + Duration::from_secs(5);
        let stopped = dedup.admit(Observation::Absent, later).unwrap();
        assert_eq!(stopped.status, PlaybackStatus::Stopped);
        assert_eq!(stopped.title, "Episode 3");
        assert_eq!(stopped.player, "mpv");
        assert_eq!(stopped.position, Duration::from_secs(90));
        assert_eq!(stopped.observed_at, later);
        assert!(!dedup.is_playing());
        assert_eq!(dedup.admit(Observation::Absent, later), None);
    }

    #[test]
    fn dedup_ignores_transitional_observations() {
        let mut dedup = Dedup::default();
        assert_eq!(dedup.admit(Observation::Transitional, now()), None);
        assert!(!dedup.is_playing());
        dedup.admit(Observation::Seen(event(PlaybackStatus::Playing, 90)), now());
        assert_eq!(dedup.admit(Observation::Transitional, now()), None);
        assert!(dedup.is_playing());
    }
}
