//! The viewing a run of playback events describes.
//!
//! Detection reports one player at a time and repeats itself; the core
//! wants to know when a different title starts and how much of it has
//! actually been watched. [`WatchSession`] keeps what that takes across
//! events: which player and title are being watched, where the player last
//! was, and the time credited so far against the recording threshold.

use std::time::{Duration, SystemTime};

use crate::{
    Decline, HistoryId, PlaybackEvent, PlaybackStatus, ProposedMatch, RecordOutcome, Watch,
    WatchOutcome,
};

/// The most one event can credit when the player never says how long the
/// file is. Position deltas are unbounded then, so wall time is the only
/// measure and a long gap has to mean something other than watching.
const MAX_STEP: Duration = Duration::from_secs(15);

/// The threshold when the duration is unknown. Taiga's flat default; with a
/// known duration the threshold is half the episode instead.
const FALLBACK_THRESHOLD: Duration = Duration::from_secs(120);

/// Across-events memory of what is being watched. The viewing is what one
/// player and title have come to. The cursor is where the player last was,
/// and it advances on every event, Stopped and untitled ones included,
/// because a position is only a delta against the one before it. The viewing
/// changes only when a titled event names another player or title: a Stopped
/// in the middle of a viewing keeps it, which is what guards a spurious
/// Stopped-then-Playing pair from detection.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct WatchSession {
    viewing: Option<Viewing>,
    cursor: Cursor,
}

/// One player and title being watched, and what watching it has come to.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Viewing {
    /// The player and the raw title.
    key: (String, String),
    /// The last non-zero duration reported for this viewing; an event that
    /// does not know it does not forget it.
    duration: Duration,
    accrued: Duration,
    /// What the viewing's recording became once it wrote one: `Recorded`,
    /// or `Undone` after an undo. Nothing records again while it is set.
    recorded: Option<RecordOutcome>,
    /// The history row this viewing has written, once it has one.
    row: Option<HistoryId>,
    /// What the viewing last put in its row, or tried to. A failed write is
    /// remembered too, so a broken table costs one notice per change of
    /// reason rather than one per event.
    logged: Option<Logged>,
}

/// Where the player last was, and when.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Cursor {
    position: Duration,
    seen: Option<SystemTime>,
}

/// What a viewing's history row says, as far as the next write cares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Logged {
    Added,
    Declined(Decline),
    Recorded,
}

impl Logged {
    fn of(outcome: WatchOutcome) -> Logged {
        match outcome {
            WatchOutcome::Added => Logged::Added,
            WatchOutcome::Declined(decline) => Logged::Declined(decline),
            WatchOutcome::Recorded(_) => Logged::Recorded,
        }
    }
}

/// What one event told the session. A first event is both a new viewing
/// and a countdown, so the two are separate facts rather than variants.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Observed {
    /// A titled player other than the one being watched.
    pub new_viewing: bool,
    /// `Some` whenever a viewing stands.
    pub progress: Option<Accrual>,
}

/// The session's side of the countdown. What the threshold came to is the
/// app's to say, since only it can run the gates and the write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Accrual {
    /// Time credited as watched so far.
    pub accrued: Duration,
    /// Half the episode when its duration is known, else a flat two minutes.
    pub threshold: Duration,
    /// What the viewing's recording became, once it has written its
    /// episode: the app shows it instead of running the gates again.
    pub recorded: Option<RecordOutcome>,
}

impl WatchSession {
    /// Picks up the viewing the persisted match describes, so a relaunch
    /// under a still-open player does not propose the same title again.
    /// When the newest history row belongs to that viewing it is picked up
    /// too, so the relaunch does not log the same watch a second time.
    pub(crate) fn resume(
        last_match: Option<&ProposedMatch>,
        newest: Option<&Watch>,
    ) -> WatchSession {
        let viewing = last_match.map(|m| {
            let key = (m.player.clone(), m.raw_title.clone());
            let newest = newest.filter(|watch| watch.player == key.0 && watch.raw_title == key.1);
            Viewing {
                row: newest.map(|watch| watch.id),
                logged: newest.map(|watch| Logged::of(watch.outcome)),
                ..Viewing::new(key)
            }
        });
        WatchSession {
            viewing,
            cursor: Cursor::default(),
        }
    }

    pub(crate) fn observe(&mut self, event: &PlaybackEvent) -> Observed {
        let titled = matches!(
            event.status,
            PlaybackStatus::Playing | PlaybackStatus::Paused
        ) && !event.title.trim().is_empty();
        let new_viewing = titled && !self.is_watching(event);
        if new_viewing {
            self.viewing = Some(Viewing::new((event.player.clone(), event.title.clone())));
        }
        if titled && let Some(viewing) = self.viewing.as_mut() {
            if !event.duration.is_zero() {
                viewing.duration = event.duration;
            }
            // The cursor belongs to what was watched before, so the event
            // that starts a viewing credits nothing.
            if !new_viewing {
                viewing.accrued += self.cursor.credit(event, viewing.duration);
            }
        }
        self.cursor = Cursor {
            position: event.position,
            seen: Some(event.observed_at),
        };
        Observed {
            new_viewing,
            progress: self.progress(),
        }
    }

    /// The episode has been written into `row`; nothing more records until
    /// the key changes.
    pub(crate) fn mark_recorded(&mut self, row: HistoryId) {
        self.wrote(row, Logged::Recorded);
        if let Some(viewing) = self.viewing.as_mut() {
            viewing.recorded = Some(RecordOutcome::Recorded(row));
        }
    }

    /// The recording in `row` was undone. The viewing that wrote it still
    /// writes nothing more, and now says its recording was undone.
    pub(crate) fn undid(&mut self, row: HistoryId) {
        if let Some(viewing) = self.viewing.as_mut()
            && viewing.recorded == Some(RecordOutcome::Recorded(row))
        {
            viewing.recorded = Some(RecordOutcome::Undone);
        }
    }

    /// The row the next write for this viewing goes into: its own, unless
    /// that row already holds a recording, which nothing overwrites.
    pub(crate) fn open_row(&self) -> Option<HistoryId> {
        let viewing = self.viewing.as_ref()?;
        viewing
            .row
            .filter(|_| viewing.logged != Some(Logged::Recorded))
    }

    pub(crate) fn logged(&self) -> Option<Logged> {
        self.viewing.as_ref()?.logged
    }

    /// `row` now says `logged`.
    pub(crate) fn wrote(&mut self, row: HistoryId, logged: Logged) {
        if let Some(viewing) = self.viewing.as_mut() {
            viewing.row = Some(row);
            viewing.logged = Some(logged);
        }
    }

    /// A write of `logged` failed; the row, if any, still says what it did.
    pub(crate) fn tried(&mut self, logged: Logged) {
        if let Some(viewing) = self.viewing.as_mut() {
            viewing.logged = Some(logged);
        }
    }

    fn is_watching(&self, event: &PlaybackEvent) -> bool {
        self.viewing
            .as_ref()
            .is_some_and(|viewing| viewing.is(event))
    }

    fn progress(&self) -> Option<Accrual> {
        let viewing = self.viewing.as_ref()?;
        let threshold = if viewing.duration.is_zero() {
            FALLBACK_THRESHOLD
        } else {
            viewing.duration / 2
        };
        Some(Accrual {
            accrued: viewing.accrued,
            threshold,
            recorded: viewing.recorded,
        })
    }
}

impl Viewing {
    fn new(key: (String, String)) -> Viewing {
        Viewing {
            key,
            duration: Duration::ZERO,
            accrued: Duration::ZERO,
            recorded: None,
            row: None,
            logged: None,
        }
    }

    fn is(&self, event: &PlaybackEvent) -> bool {
        self.key.0 == event.player && self.key.1 == event.title
    }
}

impl Cursor {
    /// Time `event` proves was watched since the cursor, in a file
    /// `duration` long. Nobody watches more file time than wall time has
    /// passed, so the position delta is capped by the wall gap: a seek
    /// credits only the seconds it took, a stalled player credits nothing,
    /// and a rewind saturates to nothing.
    fn credit(self, event: &PlaybackEvent, duration: Duration) -> Duration {
        if event.status != PlaybackStatus::Playing || event.foreground == Some(false) {
            return Duration::ZERO;
        }
        let Some(seen) = self.seen else {
            return Duration::ZERO;
        };
        // `observed_at` is wall-clock time and can step backwards.
        let wall_gap = event.observed_at.duration_since(seen).unwrap_or_default();
        if duration.is_zero() {
            wall_gap.min(MAX_STEP)
        } else {
            event.position.saturating_sub(self.position).min(wall_gap)
        }
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

    fn accrued(session: &WatchSession) -> Duration {
        session
            .progress()
            .map_or(Duration::ZERO, |progress| progress.accrued)
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
                progress: Some(Accrual {
                    accrued: Duration::ZERO,
                    threshold: secs(710),
                    recorded: None,
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
        assert_eq!(session.viewing, None);

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
        assert_eq!(session.cursor.position, Duration::from_secs(1_400));
        assert_eq!(session.cursor.seen, Some(at(2_100)));
        // Stopped does not end the viewing either: the same title coming
        // back is the same viewing.
        assert!(!session.observe(&playing("Show - 03.mkv")).new_viewing);
    }

    #[test]
    fn cursor_follows_the_last_position_and_time() {
        let mut session = WatchSession::default();
        assert_eq!(session.cursor.position, Duration::ZERO);
        assert_eq!(session.cursor.seen, None);

        session.observe(&event("Show - 03.mkv", PlaybackStatus::Playing, 305, 1_000));
        assert_eq!(session.cursor.position, Duration::from_secs(305));
        assert_eq!(session.cursor.seen, Some(at(1_000)));

        session.observe(&event("Show - 03.mkv", PlaybackStatus::Paused, 610, 1_305));
        assert_eq!(session.cursor.position, Duration::from_secs(610));
        assert_eq!(session.cursor.seen, Some(at(1_305)));

        session.observe(&event("", PlaybackStatus::Playing, 7, 1_400));
        assert_eq!(session.cursor.position, Duration::from_secs(7));
        assert_eq!(session.cursor.seen, Some(at(1_400)));
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
        let mut session = WatchSession::resume(Some(&persisted), None);
        assert!(!session.observe(&playing("Show - 03.mkv")).new_viewing);
        assert!(session.observe(&playing("Show - 04.mkv")).new_viewing);

        let mut empty = WatchSession::resume(None, None);
        assert_eq!(empty, WatchSession::default());
        assert!(empty.observe(&playing("Show - 03.mkv")).new_viewing);
    }

    fn newest_row(raw_title: &str, outcome: WatchOutcome) -> Watch {
        Watch {
            id: HistoryId(7),
            at: at(800),
            raw_title: raw_title.into(),
            parsed_title: "Show".into(),
            player: "mpv".into(),
            episode: Some(3..=3),
            link: Link::Unmatched,
            outcome,
            added_at: None,
        }
    }

    #[test]
    fn resume_picks_up_the_newest_row_only_when_it_is_the_same_viewing() {
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
        let declined = newest_row("Show - 03.mkv", WatchOutcome::Declined(Decline::NotExact));
        let session = WatchSession::resume(Some(&persisted), Some(&declined));
        assert_eq!(session.open_row(), Some(HistoryId(7)));
        assert_eq!(session.logged(), Some(Logged::Declined(Decline::NotExact)));

        let other = newest_row("Show - 02.mkv", WatchOutcome::Declined(Decline::NotExact));
        let session = WatchSession::resume(Some(&persisted), Some(&other));
        assert_eq!((session.open_row(), session.logged()), (None, None));
        assert_eq!(
            WatchSession::resume(None, Some(&declined)),
            WatchSession::default()
        );
    }

    #[test]
    fn a_recorded_row_takes_no_further_write_and_a_new_viewing_forgets_it() {
        let mut session = WatchSession::default();
        session.observe(&playing_at(100, 1_000));
        session.wrote(HistoryId(4), Logged::Declined(Decline::NotNext));
        assert_eq!(session.open_row(), Some(HistoryId(4)));
        session.mark_recorded(HistoryId(4));
        assert_eq!(session.open_row(), None);
        assert_eq!(session.logged(), Some(Logged::Recorded));

        session.observe(&event("Show - 04.mkv", PlaybackStatus::Playing, 5, 1_005));
        assert_eq!((session.open_row(), session.logged()), (None, None));
    }

    #[test]
    fn a_failed_write_is_remembered_without_a_row() {
        let mut session = WatchSession::default();
        session.observe(&playing_at(100, 1_000));
        session.tried(Logged::Declined(Decline::NotExact));
        assert_eq!(session.open_row(), None);
        assert_eq!(session.logged(), Some(Logged::Declined(Decline::NotExact)));
    }

    #[test]
    fn steady_playback_credits_the_position_delta() {
        let mut session = WatchSession::default();
        session.observe(&playing_at(100, 1_000));
        session.observe(&playing_at(101, 1_001));
        session.observe(&playing_at(160, 1_060));
        assert_eq!(accrued(&session), secs(60));
    }

    #[test]
    fn a_rewind_credits_nothing() {
        let mut session = WatchSession::default();
        session.observe(&playing_at(600, 1_000));
        session.observe(&playing_at(100, 1_001));
        assert_eq!(accrued(&session), Duration::ZERO);
        // And the cursor moved, so playback from the new spot counts.
        session.observe(&playing_at(110, 1_011));
        assert_eq!(accrued(&session), secs(10));
    }

    #[test]
    fn a_seek_credits_only_the_wall_gap() {
        let mut session = WatchSession::default();
        session.observe(&playing_at(100, 1_000));
        session.observe(&playing_at(400, 1_001));
        assert_eq!(accrued(&session), secs(1));
    }

    #[test]
    fn a_stall_credits_nothing() {
        let mut session = WatchSession::default();
        session.observe(&playing_at(100, 1_000));
        session.observe(&playing_at(100, 1_005));
        assert_eq!(accrued(&session), Duration::ZERO);
    }

    #[test]
    fn a_paused_stretch_credits_nothing() {
        let mut session = WatchSession::default();
        session.observe(&playing_at(100, 1_000));
        session.observe(&playing_at(110, 1_010));
        session.observe(&event("Show - 03.mkv", PlaybackStatus::Paused, 110, 1_011));
        session.observe(&event("Show - 03.mkv", PlaybackStatus::Paused, 110, 1_600));
        session.observe(&playing_at(110, 1_601));
        assert_eq!(accrued(&session), secs(10));
        session.observe(&playing_at(170, 1_661));
        assert_eq!(accrued(&session), secs(70));
    }

    #[test]
    fn a_background_player_credits_nothing_and_an_unknown_one_credits() {
        let mut session = WatchSession::default();
        session.observe(&playing_at(100, 1_000));
        session.observe(&PlaybackEvent {
            foreground: Some(false),
            ..playing_at(160, 1_060)
        });
        assert_eq!(accrued(&session), Duration::ZERO);
        session.observe(&PlaybackEvent {
            foreground: Some(true),
            ..playing_at(170, 1_070)
        });
        assert_eq!(accrued(&session), secs(10));
        session.observe(&PlaybackEvent {
            foreground: None,
            ..playing_at(180, 1_080)
        });
        assert_eq!(accrued(&session), secs(20));
    }

    #[test]
    fn stopped_then_playing_on_one_title_keeps_the_accrual_and_recorded() {
        let mut session = WatchSession::default();
        session.observe(&playing_at(100, 1_000));
        session.observe(&playing_at(400, 1_300));
        session.mark_recorded(HistoryId(1));
        session.observe(&event("Show - 03.mkv", PlaybackStatus::Stopped, 400, 1_301));
        assert_eq!(accrued(&session), secs(300));
        assert!(session.progress().unwrap().recorded.is_some());
        session.observe(&playing_at(401, 1_302));
        assert_eq!(accrued(&session), secs(301));
        assert!(session.progress().unwrap().recorded.is_some());
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
        assert_eq!(accrued(&session), MAX_STEP);
        session.observe(&unknown(65, 1_065));
        assert_eq!(accrued(&session), secs(20));
    }

    #[test]
    fn a_known_duration_survives_an_event_that_does_not_know_it() {
        let mut session = WatchSession::default();
        session.observe(&playing_at(0, 1_000));
        session.observe(&PlaybackEvent {
            duration: Duration::ZERO,
            ..playing_at(60, 1_060)
        });
        assert_eq!(accrued(&session), secs(60));
        assert_eq!(session.progress().unwrap().threshold, secs(710));
    }

    #[test]
    fn a_lazy_player_pushing_a_chunk_after_a_gap_is_credited_intact() {
        let mut session = WatchSession::default();
        session.observe(&playing_at(100, 1_000));
        session.observe(&playing_at(130, 1_030));
        assert_eq!(accrued(&session), secs(30));
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
        session.mark_recorded(HistoryId(1));

        let observed = session.observe(&event("Show - 04.mkv", PlaybackStatus::Playing, 5, 1_305));
        assert!(observed.new_viewing);
        assert_eq!(
            observed.progress,
            Some(Accrual {
                accrued: Duration::ZERO,
                threshold: secs(710),
                recorded: None,
            })
        );
        // Nothing from the old title's cursor leaks into the first credit.
        assert_eq!(accrued(&session), Duration::ZERO);
    }

    #[test]
    fn mark_recorded_sticks_across_same_title_events() {
        let mut session = WatchSession::default();
        session.observe(&playing_at(100, 1_000));
        session.mark_recorded(HistoryId(1));
        assert!(
            session
                .observe(&playing_at(160, 1_060))
                .progress
                .unwrap()
                .recorded
                .is_some()
        );
        assert!(
            session
                .observe(&event("Show - 03.mkv", PlaybackStatus::Paused, 160, 1_070))
                .progress
                .unwrap()
                .recorded
                .is_some()
        );
        assert!(
            session
                .observe(&event("Show - 03.mkv", PlaybackStatus::Stopped, 160, 1_080))
                .progress
                .unwrap()
                .recorded
                .is_some()
        );
        assert!(
            session
                .observe(&playing_at(220, 1_140))
                .progress
                .unwrap()
                .recorded
                .is_some()
        );
    }

    #[test]
    fn a_backwards_clock_credits_nothing() {
        let mut session = WatchSession::default();
        session.observe(&playing_at(100, 1_000));
        session.observe(&playing_at(160, 940));
        assert_eq!(accrued(&session), Duration::ZERO);
        assert_eq!(session.cursor.seen, Some(at(940)));
    }
}
