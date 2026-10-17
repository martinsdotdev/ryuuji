//! Where the playing viewing stands against its recording, worked out once
//! from the state for the two places that say it: Now playing's header in
//! full, and the status bar in a few words.

use std::ops::RangeInclusive;
use std::time::Duration;

use ryuuji_core::{AppState, Decline, HistoryId, Link, NowPlaying, RecordOutcome};

use crate::ui::{clock_text, episode_text};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Standing {
    /// An exact match counting down to its write.
    Counting {
        remaining: Duration,
        episode: Option<RangeInclusive<u32>>,
    },
    Recorded {
        watch: HistoryId,
        episode: Option<RangeInclusive<u32>>,
    },
    Undone {
        episode: Option<RangeInclusive<u32>>,
    },
    /// Nothing is written for this viewing. `refused` once the threshold was
    /// met and the write turned down, which reads "Not recorded" rather than
    /// "Not recording".
    NotRecording { why: Why, refused: bool },
}

/// How a standing reads at a glance: the colour its mark takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Tone {
    /// Counting down to the write.
    Accent,
    /// Written.
    Success,
    /// Nothing is written, and nothing went wrong.
    Neutral,
    /// A gate turned the episode down.
    Caution,
}

/// Why nothing is written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Why {
    /// A gate refused, or the person skipped the viewing. `next` is the
    /// episode the show waits for, which is what "not the next episode"
    /// leaves the person to work out.
    Declined {
        decline: Decline,
        next: Option<u32>,
    },
    /// No show in the library has the file's title.
    NoShow,
    /// A guessed show waits for the person to say it is the right one.
    Unconfirmed,
    Ignored,
}

/// The playing viewing's standing, or `None` while nothing plays: a viewing
/// left standing after the player stopped has nothing left to count.
pub(crate) fn standing(state: &AppState) -> Option<Standing> {
    if !matches!(state.now_playing, NowPlaying::Playing { .. }) {
        return None;
    }
    let progress = state.watch_progress?;
    let last = state.last_match.as_ref();
    let episode = last.and_then(|m| m.episode.clone());
    let link = last.map_or(Link::Unmatched, |m| m.link);
    let refused = progress.accrued >= progress.threshold;
    // A title no gate got past the match with is said by what the match
    // was, since "no exact match" leaves the person to guess the rest.
    let unmatched = |refused| Standing::NotRecording {
        why: match link {
            Link::Likely(_) => Why::Unconfirmed,
            Link::Exact(_) | Link::Unmatched => Why::NoShow,
        },
        refused,
    };
    Some(match progress.outcome {
        RecordOutcome::Counting => match link {
            Link::Exact(_) => Standing::Counting {
                remaining: progress.threshold.saturating_sub(progress.accrued),
                episode,
            },
            Link::Likely(_) | Link::Unmatched => unmatched(false),
        },
        RecordOutcome::Recorded(watch) => Standing::Recorded { watch, episode },
        RecordOutcome::Undone => Standing::Undone { episode },
        RecordOutcome::Declined(Decline::NotExact) => unmatched(refused),
        RecordOutcome::Declined(decline) => Standing::NotRecording {
            why: Why::Declined {
                decline,
                next: next_episode(state),
            },
            refused,
        },
        RecordOutcome::Ignored => Standing::NotRecording {
            why: Why::Ignored,
            refused: false,
        },
    })
}

/// The episode the proposal's show waits for.
fn next_episode(state: &AppState) -> Option<u32> {
    let entry = state.last_match.as_ref()?.link.entry()?;
    let entry = state.entry(entry)?;
    entry.progress.checked_add(1)
}

impl Standing {
    pub(crate) fn tone(&self) -> Tone {
        match self {
            Standing::Counting { .. } => Tone::Accent,
            Standing::Recorded { .. } => Tone::Success,
            Standing::Undone { .. }
            | Standing::NotRecording {
                why: Why::NoShow | Why::Unconfirmed | Why::Ignored,
                ..
            } => Tone::Neutral,
            Standing::NotRecording {
                why: Why::Declined { .. },
                ..
            } => Tone::Caution,
        }
    }

    /// The whole sentence, for Now playing's header.
    pub(crate) fn header(&self) -> String {
        match self {
            Standing::NotRecording { why, refused: true } => {
                format!("Not recorded: {}", why.long())
            }
            Standing::NotRecording {
                why,
                refused: false,
            } => format!("Not recording: {}", why.long()),
            Standing::Counting { .. } | Standing::Recorded { .. } | Standing::Undone { .. } => {
                self.status()
            }
        }
    }

    /// A few words, for the status bar.
    pub(crate) fn status(&self) -> String {
        match self {
            Standing::Counting { remaining, .. } => {
                format!("Records in {}", clock_text(*remaining))
            }
            Standing::Recorded { episode, .. } => match episode {
                Some(episode) => format!("Recorded {}", episode_text(episode).to_lowercase()),
                None => "Recorded".to_owned(),
            },
            Standing::Undone { episode } => match episode {
                Some(episode) => format!("Undid {}", episode_text(episode).to_lowercase()),
                None => "Undone".to_owned(),
            },
            Standing::NotRecording { why, .. } => format!("Not recording \u{b7} {}", why.short()),
        }
    }

    /// What the button that skips this viewing says it does, while there is
    /// a countdown for it to stop.
    pub(crate) fn skip_label(&self) -> Option<String> {
        let Standing::Counting { episode, .. } = self else {
            return None;
        };
        Some(match episode {
            Some(episode) => format!("Don't record {}", episode_text(episode).to_lowercase()),
            None => "Don't record this episode".to_owned(),
        })
    }
}

impl Why {
    /// The header's words. A decline says what History says, from the
    /// core's one wording; the rest are this page's own.
    fn long(self) -> String {
        match self {
            Why::Declined { .. } => self.short(),
            Why::NoShow => "no show in your library has this title".to_owned(),
            Why::Unconfirmed => "say whether this is the right show".to_owned(),
            Why::Ignored => "you ignored this file".to_owned(),
        }
    }

    /// The status bar's words. "Not the next episode" names the episode the
    /// show waits for, which History cannot: it never knew.
    fn short(self) -> String {
        match self {
            Why::Declined {
                decline: Decline::NotNext,
                next: Some(next),
            } => format!("episode {next} is next"),
            Why::Declined { decline, .. } => decline.reason().to_owned(),
            Why::NoShow => "no match".to_owned(),
            Why::Unconfirmed => "is this the show?".to_owned(),
            Why::Ignored => "ignored".to_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use ryuuji_core::{
        DataDir, LibraryEntry, NewEntry, Opened, PlaybackStatus, ProposedMatch, Store,
        WatchProgress, WatchStatus,
    };

    use super::*;

    /// A show at episode 7 with an id of its own, which only a store mints.
    fn show() -> LibraryEntry {
        let tmp = tempfile::tempdir().unwrap();
        let dir = DataDir::at(tmp.path()).unwrap();
        let Opened { mut store, .. } = Store::open(&dir).unwrap();
        store
            .add(NewEntry {
                title: "Frieren".to_owned(),
                status: WatchStatus::Watching,
                progress: 7,
                total: Some(28),
                rewatching: false,
            })
            .unwrap()
    }

    fn playing() -> NowPlaying {
        NowPlaying::Playing {
            title: "Frieren - 08.mkv".to_owned(),
            player: "mpv".to_owned(),
            status: PlaybackStatus::Playing,
            position: Duration::from_secs(693),
            duration: Duration::from_secs(1440),
        }
    }

    fn state(entry: &LibraryEntry, link: Link, accrued: u64, outcome: RecordOutcome) -> AppState {
        AppState {
            library: vec![entry.clone()],
            now_playing: playing(),
            last_match: Some(ProposedMatch {
                raw_title: "Frieren - 08.mkv".to_owned(),
                parsed_title: "Frieren".to_owned(),
                episode: Some(8..=8),
                season: None,
                release_group: None,
                link,
                player: "mpv".to_owned(),
                at: SystemTime::UNIX_EPOCH,
            }),
            watch_progress: Some(WatchProgress {
                accrued: Duration::from_secs(accrued),
                threshold: Duration::from_secs(720),
                outcome,
            }),
            ..AppState::default()
        }
    }

    fn said(state: &AppState) -> (String, String) {
        let standing = standing(state).unwrap();
        (standing.header(), standing.status())
    }

    fn pair(header: &str, status: &str) -> (String, String) {
        (header.to_owned(), status.to_owned())
    }

    #[test]
    fn nothing_stands_while_nothing_plays() {
        let entry = show();
        let stopped = AppState {
            now_playing: NowPlaying::Idle,
            ..state(&entry, Link::Exact(entry.id), 100, RecordOutcome::Counting)
        };
        assert_eq!(standing(&stopped), None);
        assert_eq!(standing(&AppState::default()), None);
    }

    #[test]
    fn an_exact_match_counts_down_and_can_be_skipped() {
        let entry = show();
        let counting = state(&entry, Link::Exact(entry.id), 693, RecordOutcome::Counting);
        assert_eq!(said(&counting), pair("Records in 0:27", "Records in 0:27"));
        assert_eq!(
            standing(&counting).unwrap().skip_label().as_deref(),
            Some("Don't record episode 8")
        );
    }

    #[test]
    fn a_file_no_show_matches_says_so_before_and_after_the_threshold() {
        let entry = show();
        let early = state(&entry, Link::Unmatched, 300, RecordOutcome::Counting);
        assert_eq!(
            said(&early),
            pair(
                "Not recording: no show in your library has this title",
                "Not recording \u{b7} no match"
            )
        );
        assert_eq!(standing(&early).unwrap().skip_label(), None);
        let refused = state(
            &entry,
            Link::Unmatched,
            800,
            RecordOutcome::Declined(Decline::NotExact),
        );
        assert_eq!(
            said(&refused).0,
            "Not recorded: no show in your library has this title"
        );
        let guess = state(&entry, Link::Likely(entry.id), 300, RecordOutcome::Counting);
        assert_eq!(
            said(&guess),
            pair(
                "Not recording: say whether this is the right show",
                "Not recording \u{b7} is this the show?"
            )
        );
    }

    #[test]
    fn a_refusal_names_the_episode_the_show_waits_for() {
        let entry = show();
        let ahead = state(
            &entry,
            Link::Exact(entry.id),
            800,
            RecordOutcome::Declined(Decline::NotNext),
        );
        assert_eq!(
            said(&ahead),
            pair(
                "Not recorded: episode 8 is next",
                "Not recording \u{b7} episode 8 is next"
            )
        );
        let skipped = state(
            &entry,
            Link::Exact(entry.id),
            300,
            RecordOutcome::Declined(Decline::Skipped),
        );
        assert_eq!(
            said(&skipped),
            pair(
                "Not recording: you skipped it",
                "Not recording \u{b7} you skipped it"
            )
        );
        // Every other refusal reads word for word as History reads it.
        for decline in [Decline::NotEpisode, Decline::Completed, Decline::PastTotal] {
            let refused = state(
                &entry,
                Link::Exact(entry.id),
                800,
                RecordOutcome::Declined(decline),
            );
            assert_eq!(said(&refused).0, decline.label());
        }
        let ignored = state(&entry, Link::Unmatched, 300, RecordOutcome::Ignored);
        assert_eq!(
            said(&ignored),
            pair(
                "Not recording: you ignored this file",
                "Not recording \u{b7} ignored"
            )
        );
    }

    #[test]
    fn a_recording_and_its_undo_name_the_episode() {
        let entry = show();
        let (_, watch) = recorded(&entry);
        let recorded = state(
            &entry,
            Link::Exact(entry.id),
            800,
            RecordOutcome::Recorded(watch),
        );
        assert_eq!(
            said(&recorded),
            pair("Recorded episode 8", "Recorded episode 8")
        );
        let undone = state(&entry, Link::Exact(entry.id), 800, RecordOutcome::Undone);
        assert_eq!(said(&undone), pair("Undid episode 8", "Undid episode 8"));
    }

    #[test]
    fn the_tone_says_at_a_glance_how_the_viewing_stands() {
        let entry = show();
        let (_, watch) = recorded(&entry);
        let exact = Link::Exact(entry.id);
        let tone = |link, accrued, outcome| {
            standing(&state(&entry, link, accrued, outcome))
                .unwrap()
                .tone()
        };
        assert_eq!(tone(exact, 300, RecordOutcome::Counting), Tone::Accent);
        assert_eq!(
            tone(exact, 800, RecordOutcome::Recorded(watch)),
            Tone::Success
        );
        assert_eq!(tone(exact, 800, RecordOutcome::Undone), Tone::Neutral);
        assert_eq!(
            tone(Link::Unmatched, 300, RecordOutcome::Counting),
            Tone::Neutral
        );
        assert_eq!(
            tone(Link::Likely(entry.id), 300, RecordOutcome::Counting),
            Tone::Neutral
        );
        assert_eq!(
            tone(Link::Unmatched, 300, RecordOutcome::Ignored),
            Tone::Neutral
        );
        // A gate turning the episode down is the one that warns, a skip
        // included: the person said no, and the header says it was refused.
        for decline in [Decline::NotNext, Decline::Skipped, Decline::Completed] {
            assert_eq!(
                tone(exact, 800, RecordOutcome::Declined(decline)),
                Tone::Caution
            );
        }
    }

    #[test]
    fn a_file_that_names_no_episode_is_said_without_one() {
        let entry = show();
        let (_, watch) = recorded(&entry);
        let unnumbered = |outcome| {
            let mut state = state(&entry, Link::Exact(entry.id), 800, outcome);
            if let Some(last) = state.last_match.as_mut() {
                last.episode = None;
            }
            standing(&state).unwrap()
        };
        assert_eq!(
            unnumbered(RecordOutcome::Recorded(watch)).status(),
            "Recorded"
        );
        assert_eq!(unnumbered(RecordOutcome::Undone).status(), "Undone");
        assert_eq!(
            unnumbered(RecordOutcome::Counting).skip_label().as_deref(),
            Some("Don't record this episode")
        );
    }

    /// A recording with an id of its own, which only a store mints.
    fn recorded(entry: &LibraryEntry) -> (LibraryEntry, HistoryId) {
        let tmp = tempfile::tempdir().unwrap();
        let dir = DataDir::at(tmp.path()).unwrap();
        let Opened { mut store, .. } = Store::open(&dir).unwrap();
        let added = store
            .add(NewEntry {
                title: entry.title.clone(),
                status: WatchStatus::Watching,
                progress: 7,
                total: Some(28),
                rewatching: false,
            })
            .unwrap();
        let recording = store
            .record(ryuuji_core::NewRecording {
                watch: None,
                entry: added.id,
                episode: 8..=8,
                raw_title: "Frieren - 08.mkv".to_owned(),
                parsed_title: "Frieren".to_owned(),
                player: "mpv".to_owned(),
            })
            .unwrap();
        (recording.entry, recording.watch.id)
    }
}
