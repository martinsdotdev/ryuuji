//! The watch log: one row per viewing that reached its threshold, whether
//! it recorded an episode or was declined, plus a viewing whose show was
//! added to the library first. Rows are never deleted; an undo marks its
//! recording and leaves the row where it was.

use std::fmt;
use std::ops::RangeInclusive;
use std::time::SystemTime;

use crate::{Decline, EntryId, LibraryEntry, Link};

/// Identity of a stored history row. Only [`Store`](crate::Store) mints
/// these.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct HistoryId(pub(crate) i64);

impl HistoryId {
    pub fn as_i64(self) -> i64 {
        self.0
    }
}

impl fmt::Display for HistoryId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// A progress write that watching is about to cause. The titles and player
/// name the proposal it was decided on, since the last match is one row and
/// gets overwritten.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewRecording {
    pub entry: EntryId,
    /// Every episode the file carries; progress moves to its end.
    pub episode: RangeInclusive<u32>,
    pub raw_title: String,
    pub parsed_title: String,
    pub player: String,
}

/// One viewing in the watch log, as stored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Watch {
    pub id: HistoryId,
    /// When the row was first written.
    pub at: SystemTime,
    pub raw_title: String,
    /// Empty for recordings made before the log kept it.
    pub parsed_title: String,
    pub player: String,
    pub episode: Option<RangeInclusive<u32>>,
    /// The proposal's link as of the row's last write.
    pub link: Link,
    pub outcome: WatchOutcome,
    /// When Add to library created the entry this watch links to.
    pub added_at: Option<SystemTime>,
}

/// What a watch came to. A recording is final; a decline is rewritten while
/// the viewing goes on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WatchOutcome {
    /// The show was added to the library and nothing has recorded or
    /// declined since.
    Added,
    Declined(Decline),
    Recorded(Recorded),
}

/// The progress write a recorded watch made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Recorded {
    pub progress_before: u32,
    pub progress: u32,
    pub at: SystemTime,
    /// Undoing marks the row and puts `progress_before` back.
    pub undone_at: Option<SystemTime>,
}

/// The newest watches, and whether older ones lie past the limit.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct HistoryPage {
    pub watches: Vec<Watch>,
    pub more: bool,
}

impl Watch {
    /// The time a row shows: when it recorded, else when it was written.
    pub fn shown_at(&self) -> SystemTime {
        match self.outcome {
            WatchOutcome::Recorded(recorded) => recorded.at,
            WatchOutcome::Added | WatchOutcome::Declined(_) => self.at,
        }
    }

    /// Whether Undo may be offered: a recording not yet undone whose entry
    /// still stands at the progress it wrote. The store refuses any other
    /// undo, so this only keeps the button from promising one.
    pub fn undoable(&self, library: &[LibraryEntry]) -> bool {
        let WatchOutcome::Recorded(recorded) = self.outcome else {
            return false;
        };
        recorded.undone_at.is_none()
            && self.link.entry().is_some_and(|id| {
                library
                    .iter()
                    .any(|entry| entry.id == id && entry.progress == recorded.progress)
            })
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, UNIX_EPOCH};

    use super::*;
    use crate::WatchStatus;

    fn secs(n: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(n)
    }

    fn show(progress: u32) -> LibraryEntry {
        LibraryEntry {
            id: EntryId(3),
            title: "Show".into(),
            status: WatchStatus::Watching,
            progress,
            total: None,
            rewatching: false,
        }
    }

    fn recorded(progress: u32, undone_at: Option<SystemTime>) -> Watch {
        Watch {
            id: HistoryId(1),
            at: secs(100),
            raw_title: "Show - 02.mkv".into(),
            parsed_title: "Show".into(),
            player: "mpv".into(),
            episode: Some(progress..=progress),
            link: Link::Exact(EntryId(3)),
            outcome: WatchOutcome::Recorded(Recorded {
                progress_before: progress - 1,
                progress,
                at: secs(160),
                undone_at,
            }),
            added_at: None,
        }
    }

    #[test]
    fn undo_is_offered_only_while_the_entry_stands_at_the_recording() {
        assert!(recorded(2, None).undoable(&[show(2)]));
        assert!(!recorded(2, None).undoable(&[show(3)]));
        assert!(!recorded(2, Some(secs(200))).undoable(&[show(1)]));
        assert!(!recorded(2, Some(secs(200))).undoable(&[show(2)]));
        assert!(!recorded(2, None).undoable(&[]));
        let declined = Watch {
            outcome: WatchOutcome::Declined(Decline::NotNext),
            ..recorded(2, None)
        };
        assert!(!declined.undoable(&[show(2)]));
    }

    #[test]
    fn a_row_shows_when_it_recorded_or_else_when_it_was_written() {
        assert_eq!(recorded(2, None).shown_at(), secs(160));
        let added = Watch {
            outcome: WatchOutcome::Added,
            ..recorded(2, None)
        };
        assert_eq!(added.shown_at(), secs(100));
    }
}
