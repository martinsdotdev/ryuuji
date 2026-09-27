//! Platform-independent application state for Ryuuji.
//!
//! This crate owns the data the shells render and the store that persists
//! it. Shells talk to it through [`Ryuuji`]: open it once, read
//! [`Ryuuji::state`], and feed user actions through [`Ryuuji::dispatch`].
//! Nothing platform-specific lives here, so it compiles everywhere the
//! future Linux shell will.

mod app;
mod data_dir;
mod diagnostics;
mod history;
mod matching;
mod playback;
mod players;
mod settings;
mod store;
mod sync;
mod tagged;
mod user_file;
mod watch;

use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

pub use app::Ryuuji;
pub use data_dir::{DataDir, DataDirError, DataDirSource};
pub use diagnostics::{ByteSize, Diagnostics, FileFacts, FileStat, ProbeFailed};
pub use history::{
    Day, HistoryId, HistoryPage, Line, NewRecording, Recorded, StatusMove, Watch, WatchOutcome,
    date, date_time, days, time_of_day, week_start,
};
// The zone history reads its local days in; a shell passes the system's.
pub use jiff::tz::TimeZone;
pub use matching::{Confidence, Link, ProposedMatch, normalize_title, similarity};
pub use playback::{PlaybackEvent, PlaybackSource, PlaybackStatus};
pub use players::{MIN_PATTERN_LEN, PlayerRow, too_short};
// Shells depend on this crate alone, so the parser reaches them through here.
pub use ryuuji_parse::{Certainty, ElementKind, Options, parse};
pub use store::{
    Added, DbError, Moved, Opened, Recording, Recovered, SchemaVersion, Settled, Store, StoreError,
    Tied,
};
pub use sync::{
    Account, Asked, Attempt, Candidate, Job, LinkState, LinkedBy, ListBase, ListEntryId, ListState,
    ListUpdate, ListUpdateId, Listing, Media, MediaId, Merge, Parked, Patch, Picker, Remote,
    Report, Service, ServiceWork, Signed, SyncState, Task, Tie, UserId, When, Work, Write, merge,
};
pub(crate) use sync::{AutoLink, Settlement, auto_link, settlement};
use tagged::tagged_enum;
pub use user_file::{FileWatch, UserFile, watch_files};

tagged_enum! {
    /// Every destination in the navigation pane, in pane order, so the
    /// variant list is the pane list. A view reached from a page rather than
    /// from the pane is a [`Detail`]. Tags are the navigation item tags.
    /// Seasons and Statistics hold a place in the pane before their pages
    /// are built.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Page {
        Library => "library", "Library",
        NowPlaying => "now-playing", "Now playing",
        History => "history", "History",
        Seasons => "seasons", "Seasons",
        Statistics => "statistics", "Statistics",
        Settings => "settings", "Settings",
    }
}

/// A view a page opens over itself. The page underneath stays selected and
/// closing the detail returns to it. Nothing routes to a detail by name, so
/// unlike a [`Page`] it needs no tag.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Detail {
    Diagnostics,
}

impl Detail {
    /// Human-readable label.
    pub fn label(self) -> &'static str {
        match self {
            Detail::Diagnostics => "Diagnostics",
        }
    }
}

tagged_enum! {
    /// Where a show sits in the user's library, in the order the UI lists
    /// them. Tags are stored in the library database.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum WatchStatus {
        Watching => "watching", "Watching",
        Completed => "completed", "Completed",
        OnHold => "on-hold", "On hold",
        Dropped => "dropped", "Dropped",
        PlanToWatch => "plan-to-watch", "Plan to watch",
    }
}

tagged_enum! {
    /// Where a show stands, with a rewatch folded in as a status of its own.
    /// A list service keeps a rewatch that way (AniList's REPEATING), so the
    /// [`WatchStatus`] and the rewatch flag read as one value here, and it
    /// is this value a recording moves and an undo puts back. Tags are
    /// stored in the history table's `status_before` and `status` columns.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum ListStatus {
        Watching => "watching", "Watching",
        Rewatching => "rewatching", "Rewatching",
        Completed => "completed", "Completed",
        OnHold => "on-hold", "On hold",
        Dropped => "dropped", "Dropped",
        PlanToWatch => "plan-to-watch", "Plan to watch",
    }
}

impl ListStatus {
    /// The status an entry stands at: a rewatch under way, else its shelf.
    pub fn of(entry: &LibraryEntry) -> ListStatus {
        if entry.rewatching {
            return ListStatus::Rewatching;
        }
        match entry.status {
            WatchStatus::Watching => ListStatus::Watching,
            WatchStatus::Completed => ListStatus::Completed,
            WatchStatus::OnHold => ListStatus::OnHold,
            WatchStatus::Dropped => ListStatus::Dropped,
            WatchStatus::PlanToWatch => ListStatus::PlanToWatch,
        }
    }

    /// The `(status, rewatching)` pair an entry stores this as.
    pub(crate) fn columns(self) -> (WatchStatus, bool) {
        match self {
            ListStatus::Watching => (WatchStatus::Watching, false),
            ListStatus::Rewatching => (WatchStatus::Completed, true),
            ListStatus::Completed => (WatchStatus::Completed, false),
            ListStatus::OnHold => (WatchStatus::OnHold, false),
            ListStatus::Dropped => (WatchStatus::Dropped, false),
            ListStatus::PlanToWatch => (WatchStatus::PlanToWatch, false),
        }
    }
}

tagged_enum! {
    /// Which colour theme the shell should request from the platform, in the
    /// order the UI lists them. Tags are written to `settings.toml`.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
    pub enum ThemePreference {
        /// Follow the operating system setting.
        #[default]
        System => "system", "System",
        Light => "light", "Light",
        Dark => "dark", "Dark",
    }
}

/// What `settings.toml` holds.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Settings {
    pub theme: ThemePreference,
}

/// Identity of a stored library entry. Only [`Store`] mints these.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EntryId(i64);

impl EntryId {
    pub fn as_i64(self) -> i64 {
        self.0
    }
}

impl fmt::Display for EntryId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// A show about to be added to the library.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewEntry {
    pub title: String,
    pub status: WatchStatus,
    /// Episodes watched so far.
    pub progress: u32,
    /// Total episode count when known.
    pub total: Option<u32>,
    /// Watching again after Completed; lets a recorded episode land on a
    /// finished show. Set from M5, never here.
    pub rewatching: bool,
}

/// One tracked show as stored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LibraryEntry {
    pub id: EntryId,
    pub title: String,
    pub status: WatchStatus,
    /// Episodes watched so far.
    pub progress: u32,
    /// Total episode count when known.
    pub total: Option<u32>,
    /// Watching again after Completed; lets a recorded episode land on a
    /// finished show. Set from M5, never here.
    pub rewatching: bool,
    /// Rewatches of this show that reached their last episode while Ryuuji
    /// kept it.
    pub rewatched: u32,
}

impl LibraryEntry {
    /// The entry as recording an episode through `last` leaves it: the rule
    /// for where a recording moves a show. Plan to watch, On hold and
    /// Dropped move to Watching, as both Taiga versions do. The last episode
    /// of a known total completes the show, and when a rewatch reaches it
    /// the rewatch ends and is counted. Anything else keeps its status: a
    /// Watching show stays Watching, and a rewatch short of its end stays
    /// one.
    pub(crate) fn after_episode(&self, last: u32) -> LibraryEntry {
        let mut entry = LibraryEntry {
            progress: last,
            ..self.clone()
        };
        if self.total == Some(last) {
            entry.status = WatchStatus::Completed;
            if self.rewatching {
                entry.rewatching = false;
                entry.rewatched = self.rewatched.saturating_add(1);
            }
        } else if !self.rewatching
            && matches!(
                self.status,
                WatchStatus::PlanToWatch | WatchStatus::OnHold | WatchStatus::Dropped
            )
        {
            entry.status = WatchStatus::Watching;
        }
        entry
    }

    /// The entry as undoing `recorded` leaves it: the inverse of
    /// [`LibraryEntry::after_episode`]. Progress goes back to what stood
    /// before. The status goes back too, but only while the show still holds
    /// the one the recording set: a status set by hand since is the person's
    /// later word, and undoing the write does not take it back. A rewatch the
    /// recording ended was counted, so putting it back uncounts it.
    pub(crate) fn after_undo(&self, recorded: &Recorded) -> LibraryEntry {
        let mut entry = LibraryEntry {
            progress: recorded.progress_before,
            ..self.clone()
        };
        if let Some(moved) = recorded.status
            && ListStatus::of(self) == moved.after
        {
            (entry.status, entry.rewatching) = moved.before.columns();
            if moved.before == ListStatus::Rewatching && moved.after == ListStatus::Completed {
                entry.rewatched = self.rewatched.saturating_sub(1);
            }
        }
        entry
    }
}

/// What the detection side currently reports.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum NowPlaying {
    /// No player activity.
    #[default]
    Idle,
    /// A player is open but nothing has been identified yet.
    Detecting,
    /// A player reported something with a title.
    Playing {
        title: String,
        player: String,
        status: PlaybackStatus,
        position: Duration,
        /// [`Duration::ZERO`] means unknown.
        duration: Duration,
    },
}

impl NowPlaying {
    /// What the shell shows for one observation.
    pub(crate) fn of(event: &PlaybackEvent) -> NowPlaying {
        match event.status {
            PlaybackStatus::Stopped => NowPlaying::Idle,
            PlaybackStatus::Playing | PlaybackStatus::Paused if event.title.trim().is_empty() => {
                NowPlaying::Detecting
            }
            PlaybackStatus::Playing | PlaybackStatus::Paused => NowPlaying::Playing {
                title: event.title.clone(),
                player: event.player.clone(),
                status: event.status,
                position: event.position,
                duration: event.duration,
            },
        }
    }
}

/// How far the standing viewing is from being recorded as an episode, and
/// what came of getting there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WatchProgress {
    /// Time credited as watched so far.
    pub accrued: Duration,
    /// Half the episode when its duration is known, else a flat two minutes.
    pub threshold: Duration,
    pub outcome: RecordOutcome,
}

/// What reaching the threshold did for the standing viewing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordOutcome {
    /// Still short of the threshold, or a write failed and the next event
    /// past it tries again.
    Counting,
    Recorded(HistoryId),
    /// The viewing's recording was undone, while it stood or while it was
    /// set aside for another player. The viewing stays recorded, so watching
    /// on writes nothing more; a relaunch starts over.
    Undone,
    /// The threshold was met and the gates refused the write. Recomputed on
    /// every event, so a decline never outlives what caused it.
    Declined(Decline),
    /// The file is on the ignore list, so no episode is counted towards and
    /// the gates never run. Said before the threshold as well as after it,
    /// because there is nothing to count down to.
    Ignored,
}

tagged_enum! {
    /// Why a viewing past its threshold wrote nothing, in the order the
    /// gates run: the name first, then the match, then the entry. Tags are
    /// stored in the history table's `reason` column.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Decline {
        /// The name says it is a preview, a trailer, an opening. Such a name
        /// carries a real show and often a real episode number, so it is
        /// refused before the match is even considered: it is true of the
        /// name whichever show it turns out to be about.
        NotEpisode => "not-episode", "Not recorded: not an episode",
        NotExact => "not-exact", "Not recorded: no exact match",
        Completed => "completed", "Not recorded: show is completed",
        NoEpisode => "no-episode", "Not recorded: no episode number",
        NotNext => "not-next", "Not recorded: not the next episode",
        /// Numbered past a known total. Taiga's `IsValidEpisodeNumber`
        /// refuses the same; such a file is more often absolute numbering
        /// than a real episode.
        PastTotal => "past-total", "Not recorded: past the show's total",
    }
}

/// Everything a shell can ask the core to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    /// Selects a pane destination, closing any open detail.
    SelectPage(Page),
    /// Opens a detail over the current page.
    OpenDetail(Detail),
    /// Returns from the open detail to the page under it.
    CloseDetail,
    AddEntry(NewEntry),
    SetProgress {
        id: EntryId,
        progress: u32,
    },
    SetStatus {
        id: EntryId,
        status: WatchStatus,
    },
    /// Starts or stops watching a finished show again. Starting one puts the
    /// show's count back to zero, so its episodes record from the first.
    SetRewatching {
        id: EntryId,
        rewatching: bool,
    },
    SetTheme(ThemePreference),
    /// A file a person edits was saved: read them all again and apply what
    /// changed. Rereading a file that did not change changes nothing.
    Reload,
    /// Drops the oldest notice.
    DismissNotice,
    /// A detection strategy or the inject control observed a player.
    Playback(PlaybackEvent),
    /// Creates a Watching entry from the unmatched last proposal, relinks
    /// it, and marks the viewing's history row as added.
    AddProposedToLibrary,
    /// Marks a recording undone and puts the entry's progress back to what
    /// it was before the write, while the entry still stands where the
    /// recording left it.
    UndoRecording(HistoryId),
    /// Says the guessed show is the right one: remembers the title, relinks
    /// the standing proposal as exact, and records at once when the viewing
    /// has already earned it.
    ConfirmProposedMatch,
    /// Says not to track the standing file: remembers its raw title, marks
    /// the viewing's history row, and unlinks the proposal at once, so the
    /// card stops naming a show without waiting for the next event.
    IgnoreFile,
    /// The way back from [`Command::IgnoreFile`]: forgets the raw title and
    /// matches the standing file against the library again. The history row
    /// saying it was ignored stays, because nothing here deletes.
    StopIgnoring,
    /// Says which show the standing file belongs to: remembers the title, so
    /// later files of that release match it on their own, and relinks the
    /// proposal. A viewing that already recorded has its episode moved onto
    /// the chosen show, leaving both rows; one that has not runs the gates
    /// at once, exactly as confirming a guess does.
    PickShow(EntryId),
    /// The sync worker says what it did or found. Every report comes through
    /// here, so what a list says reaches the library by the one reducer,
    /// like every other change.
    Synced(Report),
    /// Opens the picker over a show with the words to search the service
    /// for. The search itself is the worker's, asked through
    /// [`AppState::work`] until it answers.
    SearchList {
        entry: EntryId,
        service: Service,
        query: String,
    },
    /// The person's pick: ties the show to `media` and closes the picker.
    LinkShow {
        entry: EntryId,
        service: Service,
        media: Media,
    },
    /// Says not to keep this show in step with the service. Closes the
    /// picker if it was open.
    StopSyncing {
        entry: EntryId,
        service: Service,
    },
    /// Sends a parked show's change again.
    RetrySync {
        entry: EntryId,
        service: Service,
    },
    ClosePicker,
}

/// Something the shell should surface to the user until dismissed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Notice {
    SaveFailed {
        detail: String,
    },
    /// A file the person edits has problems. Each sentence says what was
    /// dropped; the rest of the file applies. At most one stands per file.
    FileProblem {
        file: UserFile,
        problems: Vec<String>,
    },
    /// The library file was not a readable database. It was moved to
    /// `backup` and a fresh one created.
    LibraryReset {
        backup: PathBuf,
    },
    /// The service refused the kept token, so nothing goes out until a new
    /// one is pasted. Said once per refusal; signing in or out takes it
    /// away.
    SignInRefused {
        service: Service,
    },
    /// The service refused a change to `show`, which is parked with the
    /// message until it next changes or is retried. A settle or a retry
    /// takes it away.
    ListRejected {
        service: Service,
        show: String,
        message: String,
    },
    /// The media `show` was tied to is no longer on the service, so it is
    /// back under Pick. Picking again, or stopping, takes it away.
    ListGone {
        service: Service,
        show: String,
    },
}

/// The whole application state as the shell sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppState {
    pub page: Page,
    /// The detail open over [`AppState::page`], which stays selected under it.
    pub detail: Option<Detail>,
    pub library: Vec<LibraryEntry>,
    pub now_playing: NowPlaying,
    /// The latest playback title's proposal against the library. Survives
    /// restarts via the store.
    pub last_match: Option<ProposedMatch>,
    /// Whether the file behind [`AppState::last_match`] is one the person
    /// said not to track, so Now playing offers the way back instead of a
    /// match. Read from the ignore list rather than stored on the proposal,
    /// which is about the library.
    pub ignored: bool,
    /// The standing viewing's countdown to a recorded episode and what came
    /// of it, so the shell can show both. `None` when nothing is being
    /// watched.
    pub watch_progress: Option<WatchProgress>,
    pub settings: Settings,
    /// The rows of `players.toml`, which detection lays over its built-in
    /// players.
    pub players: Vec<PlayerRow>,
    /// Oldest first; the shell shows the front one.
    pub notices: Vec<Notice>,
    /// The accounts signed in and the shows tied to them, read once at
    /// open.
    pub sync: SyncState,
}

impl Default for AppState {
    fn default() -> Self {
        AppState {
            page: Page::Library,
            detail: None,
            library: Vec::new(),
            now_playing: NowPlaying::Idle,
            last_match: None,
            ignored: false,
            watch_progress: None,
            settings: Settings::default(),
            players: Vec::new(),
            notices: Vec::new(),
            sync: SyncState::default(),
        }
    }
}

/// Joins an error and its sources with `": "`, outermost first.
pub fn error_chain(err: &dyn std::error::Error) -> String {
    let mut chain = err.to_string();
    let mut source = err.source();
    while let Some(cause) = source {
        chain.push_str(": ");
        chain.push_str(&cause.to_string());
        source = cause.source();
    }
    chain
}

#[cfg(test)]
mod tests {
    use std::time::UNIX_EPOCH;

    use super::*;

    fn show(
        status: WatchStatus,
        progress: u32,
        total: Option<u32>,
        rewatching: bool,
    ) -> LibraryEntry {
        LibraryEntry {
            id: EntryId(1),
            title: "Show".into(),
            status,
            progress,
            total,
            rewatching,
            rewatched: 2,
        }
    }

    fn standing(entry: &LibraryEntry) -> (WatchStatus, bool, u32, u32) {
        (
            entry.status,
            entry.rewatching,
            entry.rewatched,
            entry.progress,
        )
    }

    #[test]
    fn a_list_status_folds_the_shelf_and_the_rewatch_flag_into_one() {
        assert_eq!(
            ListStatus::of(&show(WatchStatus::Completed, 3, Some(12), true)),
            ListStatus::Rewatching
        );
        assert_eq!(
            ListStatus::of(&show(WatchStatus::Completed, 12, Some(12), false)),
            ListStatus::Completed
        );
        for status in ListStatus::ALL {
            let (shelf, rewatching) = status.columns();
            assert_eq!(ListStatus::of(&show(shelf, 0, None, rewatching)), status);
        }
        assert_eq!(ListStatus::Rewatching.label(), "Rewatching");
        assert_eq!(
            ListStatus::PlanToWatch.tag(),
            WatchStatus::PlanToWatch.tag()
        );
    }

    /// Every status, with and without a rewatch under way, through an
    /// episode short of the total, one with no total to be short of, and
    /// the last one.
    #[test]
    fn an_episode_moves_the_show_by_the_recording_rule() {
        use WatchStatus::*;
        for status in WatchStatus::ALL {
            for rewatching in [false, true] {
                let case = format!("{status:?}, rewatching {rewatching}");
                // Short of the end, the shelf statuses move to Watching and
                // a rewatch stays one wherever it is shelved.
                let expected = match status {
                    PlanToWatch | OnHold | Dropped if !rewatching => Watching,
                    _ => status,
                };
                let short = show(status, 4, Some(12), rewatching).after_episode(5);
                assert_eq!(standing(&short), (expected, rewatching, 2, 5), "{case}");
                let open_ended = show(status, 4, None, rewatching).after_episode(5);
                assert_eq!(
                    standing(&open_ended),
                    (expected, rewatching, 2, 5),
                    "{case}"
                );
                // The last episode finishes the show, and a rewatch with it.
                let last = show(status, 11, Some(12), rewatching).after_episode(12);
                let rewatched = if rewatching { 3 } else { 2 };
                assert_eq!(standing(&last), (Completed, false, rewatched, 12), "{case}");
            }
        }
    }

    #[test]
    fn undoing_puts_the_status_back_only_while_the_show_still_holds_it() {
        let moved = |before, after| Recorded {
            progress_before: 4,
            progress: 5,
            at: UNIX_EPOCH,
            undone_at: None,
            status: Some(StatusMove { before, after }),
        };
        let planned = moved(ListStatus::PlanToWatch, ListStatus::Watching);
        let back = show(WatchStatus::Watching, 5, Some(12), false).after_undo(&planned);
        assert_eq!(standing(&back), (WatchStatus::PlanToWatch, false, 2, 4));
        // Set On hold by hand since: the later word stands, the episode
        // still comes off.
        let held = show(WatchStatus::OnHold, 5, Some(12), false).after_undo(&planned);
        assert_eq!(standing(&held), (WatchStatus::OnHold, false, 2, 4));
        // A rewatch the recording ended comes back, and its count with it.
        let ended = Recorded {
            progress_before: 11,
            progress: 12,
            ..moved(ListStatus::Rewatching, ListStatus::Completed)
        };
        let again = show(WatchStatus::Completed, 12, Some(12), false).after_undo(&ended);
        assert_eq!(standing(&again), (WatchStatus::Completed, true, 1, 11));
        // A recording that moved nothing puts back progress alone.
        let plain = Recorded {
            status: None,
            ..planned
        };
        let alone = show(WatchStatus::Watching, 5, None, false).after_undo(&plain);
        assert_eq!(standing(&alone), (WatchStatus::Watching, false, 2, 4));
    }

    #[test]
    fn playback_status_labels() {
        assert_eq!(PlaybackStatus::Playing.label(), "Playing");
        assert_eq!(PlaybackStatus::Paused.label(), "Paused");
        assert_eq!(PlaybackStatus::Stopped.label(), "Stopped");
    }

    #[test]
    fn decline_labels_say_why_nothing_was_recorded() {
        assert_eq!(Decline::NotExact.label(), "Not recorded: no exact match");
        assert_eq!(
            Decline::Completed.label(),
            "Not recorded: show is completed"
        );
        assert_eq!(
            Decline::NoEpisode.label(),
            "Not recorded: no episode number"
        );
        assert_eq!(
            Decline::NotNext.label(),
            "Not recorded: not the next episode"
        );
        assert_eq!(
            Decline::PastTotal.label(),
            "Not recorded: past the show's total"
        );
    }

    #[test]
    fn default_state_is_empty_library_on_library_page() {
        let state = AppState::default();
        assert_eq!(state.page, Page::Library);
        assert_eq!(state.detail, None);
        assert!(state.library.is_empty());
        assert_eq!(state.now_playing, NowPlaying::Idle);
        assert_eq!(state.last_match, None);
        assert_eq!(state.watch_progress, None);
        assert_eq!(state.settings.theme, ThemePreference::System);
        assert!(state.notices.is_empty());
        assert_eq!(state.sync, SyncState::default());
    }
}
