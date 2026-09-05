//! The viewing a run of playback events describes.
//!
//! Detection reports one player at a time and repeats itself; the core
//! wants to know when a different title starts and how much of it has
//! actually been watched. [`WatchSession`] keeps what that takes across
//! events: which player and title are being watched, where the player last
//! was, and the time credited so far against the recording threshold.

use std::time::{Duration, SystemTime};

use crate::{PlaybackEvent, PlaybackStatus, ProposedMatch, WatchProgress};

/// The most one event can credit when the player never says how long the
/// file is. Position deltas are unbounded then, so wall time is the only
/// measure and a long gap has to mean something other than watching.
const MAX_STEP: Duration = Duration::from_secs(15);

/// The threshold when the duration is unknown. Taiga's flat default; with a
/// known duration the threshold is half the episode instead.
const FALLBACK_THRESHOLD: Duration = Duration::from_secs(120);

/// Across-events memory of what is being watched. The key is the viewing's
/// identity. The cursor is where the player last was, and it advances on
/// every event, Stopped and untitled ones included, because a position is
/// only a delta against the one before it. The accrual and the recorded
/// flag reset only when the key changes: a Stopped in the middle of a
/// viewing keeps both, which is what guards a spurious Stopped-then-Playing
/// pair from detection.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct WatchSession {
    key: Option<(String, String)>,
    last_position: Duration,
    last_seen: Option<SystemTime>,
    /// The last non-zero duration reported for this viewing; an event that
    /// does not know it does not forget it.
    duration: Duration,
    accrued: Duration,
    recorded: bool,
}

/// What one event told the session. A first event is both a new viewing
/// and a countdown, so the two are separate facts rather than variants.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Observed {
    /// A titled player other than the one being watched.
    pub new_viewing: bool,
    /// `Some` whenever a viewing stands.
    pub progress: Option<WatchProgress>,
}

impl WatchSession {
    /// Picks up the viewing the persisted match describes, so a relaunch
    /// under a still-open player does not propose the same title again.
    pub(crate) fn resume(last_match: Option<&ProposedMatch>) -> WatchSession {
        WatchSession {
            key: last_match.map(|m| (m.player.clone(), m.raw_title.clone())),
            ..WatchSession::default()
        }
    }

    pub(crate) fn observe(&mut self, event: &PlaybackEvent) -> Observed {
        let titled = matches!(
            event.status,
            PlaybackStatus::Playing | PlaybackStatus::Paused
        ) && !event.title.trim().is_empty();
        let new_viewing = titled && !self.is_watching(event);
        if new_viewing {
            *self = WatchSession {
                key: Some((event.player.clone(), event.title.clone())),
                ..WatchSession::default()
            };
        }
        if titled {
            if !event.duration.is_zero() {
                self.duration = event.duration;
            }
            self.accrued += self.credit(event);
        }
        self.last_position = event.position;
        self.last_seen = Some(event.observed_at);
        Observed {
            new_viewing,
            progress: self.progress(),
        }
    }

    /// The episode has been written; nothing more records until the key
    /// changes.
    pub(crate) fn mark_recorded(&mut self) {
        self.recorded = true;
    }

    fn is_watching(&self, event: &PlaybackEvent) -> bool {
        self.key
            .as_ref()
            .is_some_and(|(player, title)| *player == event.player && *title == event.title)
    }

    /// Time this event proves was watched since the cursor. Nobody watches
    /// more file time than wall time has passed, so the position delta is
    /// capped by the wall gap: a seek credits only the seconds it took, a
    /// stalled player credits nothing, and a rewind saturates to nothing.
    fn credit(&self, event: &PlaybackEvent) -> Duration {
        if event.status != PlaybackStatus::Playing || event.foreground == Some(false) {
            return Duration::ZERO;
        }
        let Some(seen) = self.last_seen else {
            return Duration::ZERO;
        };
        // `observed_at` is wall-clock time and can step backwards.
        let wall_gap = event.observed_at.duration_since(seen).unwrap_or_default();
        if self.duration.is_zero() {
            wall_gap.min(MAX_STEP)
        } else {
            event
                .position
                .saturating_sub(self.last_position)
                .min(wall_gap)
        }
    }

    fn progress(&self) -> Option<WatchProgress> {
        self.key.as_ref()?;
        let threshold = if self.duration.is_zero() {
            FALLBACK_THRESHOLD
        } else {
            self.duration / 2
        };
        Some(WatchProgress {
            accrued: self.accrued,
            threshold,
            recorded: self.recorded,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Link, PlaybackSource};

    fn at(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn secs(value: u64) -> Duration {
        Duration::from_secs(value)
    }

    fn event(title: &str, status: PlaybackStatus, position: u64, seen: u64) -> PlaybackEvent {
        PlaybackEvent {
            player: "mpv".into(),
            title: title.into(),
            status,
            position: Duration::from_secs(position),
            duration: Duration::from_secs(1420),
            observed_at: at(seen),
            source: PlaybackSource::Detected,
            foreground: None,
        }
    }

    fn playing(title: &str) -> PlaybackEvent {
        event(title, PlaybackStatus::Playing, 305, 1_000)
    }

    /// A same-title Playing event at `position` seen at `seen`.
    fn playing_at(position: u64, seen: u64) -> PlaybackEvent {
        event("Show - 03.mkv", PlaybackStatus::Playing, position, seen)
    }

    #[test]
    fn first_titled_event_starts_a_viewing() {
        let mut session = WatchSession::default();
        assert!(session.observe(&playing("Show - 03.mkv")).new_viewing);
        let mut paused = WatchSession::default();
        assert!(
            paused
                .observe(&event("Show - 03.mkv", PlaybackStatus::Paused, 0, 1_000))
                .new_viewing
        );
    }

    #[test]
    fn first_event_is_a_new_viewing_and_a_countdown() {
        let mut session = WatchSession::default();
        let observed = session.observe(&playing("Show - 03.mkv"));
        assert_eq!(
            observed,
            Observed {
                new_viewing: true,
                progress: Some(WatchProgress {
                    accrued: Duration::ZERO,
                    threshold: secs(710),
                    recorded: false,
                }),
            }
        );
    }

    #[test]
    fn same_title_continues_the_viewing() {
        let mut session = WatchSession::default();
        session.observe(&playing("Show - 03.mkv"));
        assert!(!session.observe(&playing_at(400, 1_060)).new_viewing);
        assert!(
            !session
                .observe(&event("Show - 03.mkv", PlaybackStatus::Paused, 400, 1_070))
                .new_viewing
        );
    }

    #[test]
    fn title_change_starts_a_new_viewing() {
        let mut session = WatchSession::default();
        session.observe(&playing("Show - 03.mkv"));
        assert!(session.observe(&playing("Show - 04.mkv")).new_viewing);
        assert!(!session.observe(&playing("Show - 04.mkv")).new_viewing);
    }

    #[test]
    fn player_change_starts_a_new_viewing() {
        let mut session = WatchSession::default();
        session.observe(&playing("Show - 03.mkv"));
        let other = PlaybackEvent {
            player: "vlc".into(),
            ..playing("Show - 03.mkv")
        };
        assert!(session.observe(&other).new_viewing);
        assert!(!session.observe(&other).new_viewing);
    }

    #[test]
    fn blank_title_neither_starts_nor_ends_a_viewing() {
        let mut session = WatchSession::default();
        let observed = session.observe(&playing(""));
        assert_eq!(
            observed,
            Observed {
                new_viewing: false,
                progress: None
            }
        );
        assert!(!session.observe(&playing("  \t")).new_viewing);
        assert_eq!(session.key, None);

        session.observe(&playing("Show - 03.mkv"));
        let observed = session.observe(&playing(""));
        assert!(!observed.new_viewing);
        assert!(observed.progress.is_some());
        assert!(!session.observe(&playing("Show - 03.mkv")).new_viewing);
    }

    #[test]
    fn stopped_keeps_the_cursor() {
        let mut session = WatchSession::default();
        session.observe(&playing("Show - 03.mkv"));
        let stopped = event("Show - 03.mkv", PlaybackStatus::Stopped, 1_400, 2_100);
        assert!(!session.observe(&stopped).new_viewing);
        assert_eq!(session.last_position, Duration::from_secs(1_400));
        assert_eq!(session.last_seen, Some(at(2_100)));
        // Stopped does not end the viewing either: the same title coming
        // back is the same viewing.
        assert!(!session.observe(&playing("Show - 03.mkv")).new_viewing);
    }

    #[test]
    fn cursor_follows_the_last_position_and_time() {
        let mut session = WatchSession::default();
        assert_eq!(session.last_position, Duration::ZERO);
        assert_eq!(session.last_seen, None);

        session.observe(&event("Show - 03.mkv", PlaybackStatus::Playing, 305, 1_000));
        assert_eq!(session.last_position, Duration::from_secs(305));
        assert_eq!(session.last_seen, Some(at(1_000)));

        session.observe(&event("Show - 03.mkv", PlaybackStatus::Paused, 610, 1_305));
        assert_eq!(session.last_position, Duration::from_secs(610));
        assert_eq!(session.last_seen, Some(at(1_305)));

        session.observe(&event("", PlaybackStatus::Playing, 7, 1_400));
        assert_eq!(session.last_position, Duration::from_secs(7));
        assert_eq!(session.last_seen, Some(at(1_400)));
    }

    #[test]
    fn resume_seeds_the_key_from_a_persisted_match() {
        let persisted = ProposedMatch {
            raw_title: "Show - 03.mkv".into(),
            parsed_title: "Show".into(),
            episode: Some(3..=3),
            season: None,
            release_group: None,
            link: Link::Unmatched,
            player: "mpv".into(),
            at: at(900),
        };
        let mut session = WatchSession::resume(Some(&persisted));
        assert!(!session.observe(&playing("Show - 03.mkv")).new_viewing);
        assert!(session.observe(&playing("Show - 04.mkv")).new_viewing);

        let mut empty = WatchSession::resume(None);
        assert_eq!(empty, WatchSession::default());
        assert!(empty.observe(&playing("Show - 03.mkv")).new_viewing);
    }

    #[test]
    fn steady_playback_credits_the_position_delta() {
        let mut session = WatchSession::default();
        session.observe(&playing_at(100, 1_000));
        session.observe(&playing_at(101, 1_001));
        session.observe(&playing_at(160, 1_060));
        assert_eq!(session.accrued, secs(60));
    }

    #[test]
    fn a_rewind_credits_nothing() {
        let mut session = WatchSession::default();
        session.observe(&playing_at(600, 1_000));
        session.observe(&playing_at(100, 1_001));
        assert_eq!(session.accrued, Duration::ZERO);
        // And the cursor moved, so playback from the new spot counts.
        session.observe(&playing_at(110, 1_011));
        assert_eq!(session.accrued, secs(10));
    }

    #[test]
    fn a_seek_credits_only_the_wall_gap() {
        let mut session = WatchSession::default();
        session.observe(&playing_at(100, 1_000));
        session.observe(&playing_at(400, 1_001));
        assert_eq!(session.accrued, secs(1));
    }

    #[test]
    fn a_stall_credits_nothing() {
        let mut session = WatchSession::default();
        session.observe(&playing_at(100, 1_000));
        session.observe(&playing_at(100, 1_005));
        assert_eq!(session.accrued, Duration::ZERO);
    }

    #[test]
    fn a_paused_stretch_credits_nothing() {
        let mut session = WatchSession::default();
        session.observe(&playing_at(100, 1_000));
        session.observe(&playing_at(110, 1_010));
        session.observe(&event("Show - 03.mkv", PlaybackStatus::Paused, 110, 1_011));
        session.observe(&event("Show - 03.mkv", PlaybackStatus::Paused, 110, 1_600));
        session.observe(&playing_at(110, 1_601));
        assert_eq!(session.accrued, secs(10));
        session.observe(&playing_at(170, 1_661));
        assert_eq!(session.accrued, secs(70));
    }

    #[test]
    fn a_background_player_credits_nothing_and_an_unknown_one_credits() {
        let mut session = WatchSession::default();
        session.observe(&playing_at(100, 1_000));
        session.observe(&PlaybackEvent {
            foreground: Some(false),
            ..playing_at(160, 1_060)
        });
        assert_eq!(session.accrued, Duration::ZERO);
        session.observe(&PlaybackEvent {
            foreground: Some(true),
            ..playing_at(170, 1_070)
        });
        assert_eq!(session.accrued, secs(10));
        session.observe(&PlaybackEvent {
            foreground: None,
            ..playing_at(180, 1_080)
        });
        assert_eq!(session.accrued, secs(20));
    }

    #[test]
    fn stopped_then_playing_on_one_title_keeps_the_accrual_and_recorded() {
        let mut session = WatchSession::default();
        session.observe(&playing_at(100, 1_000));
        session.observe(&playing_at(400, 1_300));
        session.recorded = true;
        session.observe(&event("Show - 03.mkv", PlaybackStatus::Stopped, 400, 1_301));
        assert_eq!(session.accrued, secs(300));
        assert!(session.recorded);
        session.observe(&playing_at(401, 1_302));
        assert_eq!(session.accrued, secs(301));
        assert!(session.recorded);
    }

    #[test]
    fn unknown_duration_caps_each_step_at_fifteen_seconds() {
        let unknown = |position, seen| PlaybackEvent {
            duration: Duration::ZERO,
            ..playing_at(position, seen)
        };
        let mut session = WatchSession::default();
        session.observe(&unknown(0, 1_000));
        session.observe(&unknown(60, 1_060));
        assert_eq!(session.accrued, MAX_STEP);
        session.observe(&unknown(65, 1_065));
        assert_eq!(session.accrued, secs(20));
    }

    #[test]
    fn a_known_duration_survives_an_event_that_does_not_know_it() {
        let mut session = WatchSession::default();
        session.observe(&playing_at(0, 1_000));
        session.observe(&PlaybackEvent {
            duration: Duration::ZERO,
            ..playing_at(60, 1_060)
        });
        assert_eq!(session.accrued, secs(60));
        assert_eq!(session.progress().unwrap().threshold, secs(710));
    }

    #[test]
    fn a_lazy_player_pushing_a_chunk_after_a_gap_is_credited_intact() {
        let mut session = WatchSession::default();
        session.observe(&playing_at(100, 1_000));
        session.observe(&playing_at(130, 1_030));
        assert_eq!(session.accrued, secs(30));
    }

    #[test]
    fn threshold_is_half_a_known_duration_and_two_minutes_otherwise() {
        let mut known = WatchSession::default();
        let progress = known.observe(&playing_at(0, 1_000)).progress.unwrap();
        assert_eq!(progress.threshold, secs(710));

        let mut unknown = WatchSession::default();
        let progress = unknown
            .observe(&PlaybackEvent {
                duration: Duration::ZERO,
                ..playing_at(0, 1_000)
            })
            .progress
            .unwrap();
        assert_eq!(progress.threshold, FALLBACK_THRESHOLD);
    }

    #[test]
    fn a_new_key_resets_the_accrual_and_recorded() {
        let mut session = WatchSession::default();
        session.observe(&playing_at(100, 1_000));
        session.observe(&playing_at(400, 1_300));
        session.recorded = true;

        let observed = session.observe(&event("Show - 04.mkv", PlaybackStatus::Playing, 5, 1_305));
        assert!(observed.new_viewing);
        assert_eq!(
            observed.progress,
            Some(WatchProgress {
                accrued: Duration::ZERO,
                threshold: secs(710),
                recorded: false,
            })
        );
        // Nothing from the old title's cursor leaks into the first credit.
        assert_eq!(session.accrued, Duration::ZERO);
    }

    #[test]
    fn mark_recorded_sticks_across_same_title_events() {
        let mut session = WatchSession::default();
        session.observe(&playing_at(100, 1_000));
        session.mark_recorded();
        assert!(
            session
                .observe(&playing_at(160, 1_060))
                .progress
                .unwrap()
                .recorded
        );
        assert!(
            session
                .observe(&event("Show - 03.mkv", PlaybackStatus::Paused, 160, 1_070))
                .progress
                .unwrap()
                .recorded
        );
        assert!(
            session
                .observe(&event("Show - 03.mkv", PlaybackStatus::Stopped, 160, 1_080))
                .progress
                .unwrap()
                .recorded
        );
        assert!(
            session
                .observe(&playing_at(220, 1_140))
                .progress
                .unwrap()
                .recorded
        );
    }

    #[test]
    fn a_backwards_clock_credits_nothing() {
        let mut session = WatchSession::default();
        session.observe(&playing_at(100, 1_000));
        session.observe(&playing_at(160, 940));
        assert_eq!(session.accrued, Duration::ZERO);
        assert_eq!(session.last_seen, Some(at(940)));
    }
}
