//! The pure half of every strategy: what one refresh saw in, at most one
//! normalized [`PlaybackEvent`] out. Nothing here touches a platform API, so
//! it is tested on every platform.

use std::time::{Duration, SystemTime};

use ryuuji_core::{PlaybackEvent, PlaybackSource, PlaybackStatus};

use crate::SessionFacts;
use crate::players::Player;

/// What a session holds, in the terms every strategy can express. The
/// platform's own vocabulary stops at the strategy: SMTC's six values and
/// MPRIS's three strings both arrive here as these four.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SessionState {
    Playing,
    Paused,
    Stopped,
    /// The session exists but has said nothing yet: SMTC's Opened and
    /// Changing, an MPRIS name that owns the bus before its properties
    /// arrive. Never an observation.
    Settling,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SessionSnapshot {
    pub title: String,
    pub state: SessionState,
    pub start: Duration,
    pub end: Duration,
    pub position: Duration,
    /// When the player last wrote the timeline, or `None` if it never has.
    pub updated: Option<SystemTime>,
}

pub(crate) struct Matched<'a> {
    pub player: &'a Player,
    pub snapshot: SessionSnapshot,
}

/// Everything one refresh saw. The front is read once per refresh, not
/// once per session, so two sessions of one player agree about it.
pub(crate) struct Reading<'a> {
    pub front: Front,
    pub sessions: Vec<Seen<'a>>,
}

/// One session, whether or not anything can be done with it. The tracking
/// hangs off the row, so a strategy cannot watch a session without also
/// listing it for Diagnostics.
pub(crate) struct Seen<'a> {
    pub app_id: String,
    /// The platform's own word for the state.
    pub status: String,
    pub tracking: Tracking<'a>,
}

pub(crate) enum Tracking<'a> {
    /// No table entry claimed this session's id.
    Unknown,
    /// A player a person turned off claimed it, so it is not watched.
    Hidden(&'a Player),
    /// The table claimed it, but the platform would not say what it holds
    /// this refresh. Keeps the observation out of `Absent`.
    Unreadable(&'a Player),
    Watched(Matched<'a>),
}

impl Seen<'_> {
    /// The Diagnostics row. The title and the player come off the tracking,
    /// so an unmatched session cannot carry either.
    pub(crate) fn facts(&self) -> SessionFacts {
        let (title, player) = match &self.tracking {
            Tracking::Unknown => (String::new(), None),
            Tracking::Hidden(player) | Tracking::Unreadable(player) => {
                (String::new(), Some(player.name.clone()))
            }
            Tracking::Watched(matched) => (
                matched.snapshot.title.clone(),
                Some(matched.player.name.clone()),
            ),
        };
        SessionFacts {
            app_id: self.app_id.clone(),
            title,
            status: self.status.clone(),
            player,
            hidden: matches!(self.tracking, Tracking::Hidden(_)),
        }
    }
}

/// Prefer the Playing session, else the first.
pub(crate) fn choose<'a, 'b>(matched: &'b [Matched<'a>]) -> Option<&'b Matched<'a>> {
    matched
        .iter()
        .find(|m| m.snapshot.state == SessionState::Playing)
        .or_else(|| matched.first())
}

/// What sat in front of everything else at one refresh. Ryuuji's own
/// window is `Own`, not `Exe`, so looking at the countdown does not read as
/// looking away from the player.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Front {
    Unknown,
    Own,
    /// The lowercased file name of the process that owns the window.
    Exe(String),
}

impl Front {
    /// Every packaged app's window belongs to ApplicationFrameHost, so that
    /// name says nothing about which app is in front.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub(crate) fn from_reading(own: bool, exe: Option<String>) -> Front {
        if own {
            return Front::Own;
        }
        match exe.map(|name| name.to_lowercase()) {
            Some(name) if name != "applicationframehost.exe" => Front::Exe(name),
            _ => Front::Unknown,
        }
    }

    /// Whether `player` owns the front window. `None` when nothing can be
    /// said: the front is unknown or Ryuuji's own, or the player names no
    /// executables and would otherwise never read as in front.
    pub(crate) fn of(&self, player: &Player) -> Option<bool> {
        match self {
            Front::Exe(name) if !player.executables.is_empty() => {
                Some(player.executables.contains(name))
            }
            _ => None,
        }
    }
}

/// A settling session is not an observation yet. Times are relative to the
/// timeline start.
pub(crate) fn normalize(
    matched: &Matched<'_>,
    now: SystemTime,
    front: &Front,
) -> Option<PlaybackEvent> {
    let snapshot = &matched.snapshot;
    let status = match snapshot.state {
        SessionState::Stopped => PlaybackStatus::Stopped,
        SessionState::Playing => PlaybackStatus::Playing,
        SessionState::Paused => PlaybackStatus::Paused,
        SessionState::Settling => return None,
    };
    Some(PlaybackEvent {
        player: matched.player.name.clone(),
        title: snapshot.title.clone(),
        status,
        position: advanced_position(snapshot, now).saturating_sub(snapshot.start),
        duration: snapshot.end.saturating_sub(snapshot.start),
        observed_at: now,
        source: PlaybackSource::Detected,
        foreground: front.of(matched.player),
    })
}

/// Chromium writes a tab's timeline only when playback starts, pauses or
/// seeks, so the position it reports goes stale while the video plays on. A
/// playing session is moved on by the time since the timeline was written,
/// and stops at the end of the item when its length is known. The playback
/// rate is not read, because accrual already caps credit at wall time.
/// Players that rewrite the timeline every second gain under a second.
fn advanced_position(snapshot: &SessionSnapshot, now: SystemTime) -> Duration {
    if snapshot.state != SessionState::Playing {
        return snapshot.position;
    }
    let Some(updated) = snapshot.updated else {
        return snapshot.position;
    };
    let moved = snapshot
        .position
        .saturating_add(now.duration_since(updated).unwrap_or_default());
    if snapshot.end > snapshot.start {
        moved.min(snapshot.end)
    } else {
        moved
    }
}

pub(crate) enum Observation {
    /// No session matched the player table.
    Absent,
    /// The chosen session is between states and says nothing yet.
    Transitional,
    Seen(PlaybackEvent),
}

/// How long a session that is merely paused waits while another matched
/// session is still unreadable. A player publishes its media properties
/// within about two seconds of starting, measured on 2026-09-15, and the
/// wait is only observable at poll boundaries, so this is that measurement
/// and one poll over.
pub(crate) const SETTLE: Duration = Duration::from_secs(3);

/// `unreadable_since` is when the current run of matched sessions whose
/// media or timeline could not be read began, and `None` when every matched
/// session read this refresh. Such a session keeps the observation out of
/// `Absent`, since one that exists but is not ready has not vanished, and
/// for [`SETTLE`] it also holds back a session that is merely paused: at
/// launch a player's session is unreadable for a moment while a paused
/// browser tab reads fine, and reporting the tab first shows it as what is
/// playing. A playing session is reported whatever is unreadable.
pub(crate) fn observe(
    matched: &[Matched<'_>],
    unreadable_since: Option<SystemTime>,
    now: SystemTime,
    front: &Front,
) -> Observation {
    match choose(matched) {
        Some(chosen)
            if chosen.snapshot.state != SessionState::Playing
                && settling(unreadable_since, now) =>
        {
            Observation::Transitional
        }
        Some(chosen) => {
            normalize(chosen, now, front).map_or(Observation::Transitional, Observation::Seen)
        }
        None if unreadable_since.is_some() => Observation::Transitional,
        None => Observation::Absent,
    }
}

/// Whether the unreadable run is young enough to keep waiting on.
fn settling(since: Option<SystemTime>, now: SystemTime) -> bool {
    since.is_some_and(|since| now.duration_since(since).unwrap_or_default() < SETTLE)
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

    /// A player whose executable is its lowercased name plus `.exe`.
    fn player(name: &str) -> Player {
        Player {
            name: name.to_owned(),
            smtc_app_ids: Vec::new(),
            mpris_ids: Vec::new(),
            executables: vec![format!("{}.exe", name.to_lowercase())],
            hidden: false,
        }
    }

    fn exe(name: &str) -> Front {
        Front::Exe(name.to_owned())
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

    /// A playing snapshot whose timeline was written `secs` before now.
    fn written_ago(secs: u64) -> SessionSnapshot {
        SessionSnapshot {
            updated: Some(now() - Duration::from_secs(secs)),
            ..snapshot(SessionState::Playing)
        }
    }

    fn position_of(snapshot: SessionSnapshot) -> Duration {
        let mpv = player("mpv");
        let matched = Matched {
            player: &mpv,
            snapshot,
        };
        normalize(&matched, now(), &Front::Unknown)
            .unwrap()
            .position
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
    fn choose_prefers_the_playing_session() {
        let (mpv, vlc) = (player("mpv"), player("VLC"));
        let matched = [
            Matched {
                player: &mpv,
                snapshot: snapshot(SessionState::Paused),
            },
            Matched {
                player: &vlc,
                snapshot: snapshot(SessionState::Playing),
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
                snapshot: snapshot(SessionState::Paused),
            },
            Matched {
                player: &vlc,
                snapshot: snapshot(SessionState::Stopped),
            },
        ];
        assert_eq!(choose(&matched).unwrap().player.name, "mpv");
        assert!(choose(&[]).is_none());
    }

    #[test]
    fn normalize_maps_each_settled_state() {
        let mpv = player("mpv");
        for (state, status) in [
            (SessionState::Stopped, PlaybackStatus::Stopped),
            (SessionState::Playing, PlaybackStatus::Playing),
            (SessionState::Paused, PlaybackStatus::Paused),
        ] {
            let matched = Matched {
                player: &mpv,
                snapshot: snapshot(state),
            };
            assert_eq!(
                normalize(&matched, now(), &Front::Unknown),
                Some(event(status, 90))
            );
        }
    }

    #[test]
    fn normalize_skips_a_settling_session() {
        let mpv = player("mpv");
        let matched = Matched {
            player: &mpv,
            snapshot: snapshot(SessionState::Settling),
        };
        assert_eq!(normalize(&matched, now(), &Front::Unknown), None);
    }

    #[test]
    fn normalize_subtracts_start_time_and_saturates() {
        let mpv = player("mpv");
        let mut shifted = snapshot(SessionState::Playing);
        shifted.start = Duration::from_secs(100);
        shifted.end = Duration::from_secs(400);
        shifted.position = Duration::from_secs(130);
        let matched = Matched {
            player: &mpv,
            snapshot: shifted.clone(),
        };
        let normalized = normalize(&matched, now(), &Front::Unknown).unwrap();
        assert_eq!(normalized.position, Duration::from_secs(30));
        assert_eq!(normalized.duration, Duration::from_secs(300));

        shifted.position = Duration::from_secs(20);
        shifted.end = Duration::from_secs(50);
        let matched = Matched {
            player: &mpv,
            snapshot: shifted,
        };
        let normalized = normalize(&matched, now(), &Front::Unknown).unwrap();
        assert_eq!(normalized.position, Duration::ZERO);
        assert_eq!(normalized.duration, Duration::ZERO);
    }

    #[test]
    fn a_playing_position_moves_on_by_the_time_since_the_timeline_was_written() {
        assert_eq!(position_of(written_ago(30)), Duration::from_secs(120));
    }

    #[test]
    fn a_moved_position_stops_at_the_end_of_the_item() {
        let near_end = SessionSnapshot {
            position: Duration::from_secs(1430),
            ..written_ago(60)
        };
        assert_eq!(position_of(near_end), Duration::from_secs(1440));
    }

    #[test]
    fn a_position_moves_past_any_end_when_the_length_is_unknown() {
        let unknown_length = SessionSnapshot {
            end: Duration::ZERO,
            ..written_ago(30)
        };
        assert_eq!(position_of(unknown_length), Duration::from_secs(120));
    }

    #[test]
    fn a_paused_or_never_written_timeline_does_not_move() {
        let paused = SessionSnapshot {
            state: SessionState::Paused,
            ..written_ago(30)
        };
        let never_written = SessionSnapshot {
            updated: None,
            ..written_ago(30)
        };
        for snapshot in [paused, never_written] {
            assert_eq!(position_of(snapshot), Duration::from_secs(90));
        }
    }

    #[test]
    fn a_timeline_written_after_now_does_not_move_the_position_back() {
        let from_the_future = SessionSnapshot {
            updated: Some(now() + Duration::from_secs(30)),
            ..snapshot(SessionState::Playing)
        };
        assert_eq!(position_of(from_the_future), Duration::from_secs(90));
    }

    fn seen<'a>(app_id: &str, tracking: Tracking<'a>) -> Seen<'a> {
        Seen {
            app_id: app_id.to_owned(),
            status: "Playing".to_owned(),
            tracking,
        }
    }

    #[test]
    fn the_row_carries_a_title_only_for_a_watched_session_and_a_player_for_any_matched_one() {
        let mpv = player("mpv");
        let unknown = seen("Spotify.exe", Tracking::Unknown).facts();
        assert_eq!(
            (unknown.title.as_str(), unknown.player.as_deref()),
            ("", None)
        );
        let unreadable = seen("mpv.exe", Tracking::Unreadable(&mpv)).facts();
        assert_eq!(
            (unreadable.title.as_str(), unreadable.player.as_deref()),
            ("", Some("mpv"))
        );
        assert!(!unreadable.hidden);
        let hidden = seen("mpv.exe", Tracking::Hidden(&mpv)).facts();
        assert_eq!(
            (
                hidden.title.as_str(),
                hidden.player.as_deref(),
                hidden.hidden
            ),
            ("", Some("mpv"), true)
        );
        let watched = seen(
            "mpv.exe",
            Tracking::Watched(Matched {
                player: &mpv,
                snapshot: snapshot(SessionState::Playing),
            }),
        )
        .facts();
        assert_eq!(
            (watched.title.as_str(), watched.player.as_deref()),
            ("Episode 3", Some("mpv"))
        );
        assert_eq!(watched.app_id, "mpv.exe");
        assert_eq!(watched.status, "Playing");
    }

    #[test]
    fn front_of_compares_the_whole_file_name_against_the_players_executables() {
        let mpv = player("mpv");
        assert_eq!(exe("mpv.exe").of(&mpv), Some(true));
        assert_eq!(exe("notepad.exe").of(&mpv), Some(false));
        assert_eq!(exe("some-mpv-skin.exe").of(&mpv), Some(false));
        assert_eq!(Front::Unknown.of(&mpv), None);
        assert_eq!(Front::Own.of(&mpv), None);
    }

    #[test]
    fn front_of_a_player_without_executables_is_unknown_never_behind() {
        let bare = Player {
            executables: Vec::new(),
            ..player("mpv")
        };
        assert_eq!(exe("mpv.exe").of(&bare), None);
        assert_eq!(exe("notepad.exe").of(&bare), None);
    }

    #[test]
    fn from_reading_lowercases_and_maps_own_and_the_frame_host_to_unknowns() {
        assert_eq!(
            Front::from_reading(false, Some("MPV.EXE".to_owned())),
            exe("mpv.exe")
        );
        assert_eq!(
            Front::from_reading(true, Some("ryuuji.exe".to_owned())),
            Front::Own
        );
        assert_eq!(Front::from_reading(false, None), Front::Unknown);
        assert_eq!(
            Front::from_reading(false, Some("ApplicationFrameHost.exe".to_owned())),
            Front::Unknown
        );
    }

    #[test]
    fn normalize_carries_the_front() {
        let mpv = player("mpv");
        let matched = Matched {
            player: &mpv,
            snapshot: snapshot(SessionState::Playing),
        };
        for (front, foreground) in [
            (exe("mpv.exe"), Some(true)),
            (exe("vlc.exe"), Some(false)),
            (Front::Own, None),
            (Front::Unknown, None),
        ] {
            let event = normalize(&matched, now(), &front).unwrap();
            assert_eq!(event.foreground, foreground, "{front:?}");
        }
    }

    #[test]
    fn two_instances_of_one_player_both_read_as_in_front() {
        let mpv = player("mpv");
        let matched = [
            Matched {
                player: &mpv,
                snapshot: snapshot(SessionState::Playing),
            },
            Matched {
                player: &mpv,
                snapshot: snapshot(SessionState::Paused),
            },
        ];
        for m in &matched {
            let event = normalize(m, now(), &exe("mpv.exe")).unwrap();
            assert_eq!(event.foreground, Some(true));
        }
    }

    #[test]
    fn unreadable_matched_session_is_transitional_not_absent() {
        assert!(matches!(
            observe(&[], Some(now()), now(), &Front::Unknown),
            Observation::Transitional
        ));
        let mpv = player("mpv");
        let matched = [Matched {
            player: &mpv,
            snapshot: snapshot(SessionState::Playing),
        }];
        assert!(matches!(
            observe(&matched, Some(now()), now(), &Front::Unknown),
            Observation::Seen(_)
        ));
    }

    #[test]
    fn no_sessions_is_absent() {
        assert!(matches!(
            observe(&[], None, now(), &Front::Unknown),
            Observation::Absent
        ));
    }

    #[test]
    fn a_paused_candidate_waits_while_a_matched_session_is_still_unreadable() {
        let mpv = player("mpv");
        let matched = [Matched {
            player: &mpv,
            snapshot: snapshot(SessionState::Paused),
        }];
        assert!(matches!(
            observe(&matched, Some(now()), now(), &Front::Unknown),
            Observation::Transitional
        ));
    }

    #[test]
    fn a_paused_candidate_is_reported_once_the_settling_window_has_passed() {
        let mpv = player("mpv");
        let matched = [Matched {
            player: &mpv,
            snapshot: snapshot(SessionState::Paused),
        }];
        assert!(matches!(
            observe(&matched, Some(now() - SETTLE), now(), &Front::Unknown),
            Observation::Seen(_)
        ));
    }

    #[test]
    fn a_playing_candidate_is_reported_however_long_a_session_has_been_unreadable() {
        let mpv = player("mpv");
        let matched = [Matched {
            player: &mpv,
            snapshot: snapshot(SessionState::Playing),
        }];
        let hour = Duration::from_secs(3_600);
        assert!(matches!(
            observe(&matched, Some(now() - hour), now(), &Front::Unknown),
            Observation::Seen(_)
        ));
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
    fn dedup_passes_a_foreground_flip_at_the_same_position() {
        let mut dedup = Dedup::default();
        let front = PlaybackEvent {
            foreground: Some(true),
            ..event(PlaybackStatus::Playing, 90)
        };
        dedup.admit(Observation::Seen(front.clone()), now());
        let behind = PlaybackEvent {
            foreground: Some(false),
            ..front
        };
        assert_eq!(
            dedup.admit(Observation::Seen(behind.clone()), now()),
            Some(behind)
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
