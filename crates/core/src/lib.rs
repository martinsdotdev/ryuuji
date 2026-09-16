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
mod settings;
mod store;
mod tagged;
mod watch;

use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

pub use app::Ryuuji;
pub use data_dir::{DataDir, DataDirError, DataDirSource};
pub use diagnostics::{ByteSize, Diagnostics, FileFacts, FileStat, ProbeFailed};
pub use history::{
    Day, HistoryId, HistoryPage, NewRecording, Recorded, Watch, WatchOutcome, date_time, days,
    time_of_day, week_start,
};
// The zone history reads its local days in; a shell passes the system's.
pub use jiff::tz::TimeZone;
pub use matching::{Confidence, Link, ProposedMatch, normalize_title, similarity};
pub use playback::{PlaybackEvent, PlaybackSource, PlaybackStatus};
// Shells depend on this crate alone, so the parser reaches them through here.
pub use ryuuji_parse::{Certainty, ElementKind, Options, parse};
pub use store::{Added, DbError, Opened, Recording, Recovered, SchemaVersion, Store, StoreError};
use tagged::tagged_enum;

tagged_enum! {
    /// Every destination in the navigation pane, in pane order, so the
    /// variant list is the pane list. A view reached from a page rather than
    /// from the pane is a [`Detail`]. Tags are the navigation item tags.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Page {
        Library => "library", "Library",
        NowPlaying => "now-playing", "Now playing",
        History => "history", "History",
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

/// Everything the user can configure, as persisted in `settings.toml`.
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
}

tagged_enum! {
    /// Why a viewing past its threshold wrote nothing, in the order the
    /// gates run: the match first, then the entry. Tags are stored in the
    /// history table's `reason` column.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Decline {
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
    SetTheme(ThemePreference),
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
}

/// Something the shell should surface to the user until dismissed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Notice {
    SaveFailed {
        detail: String,
    },
    /// `settings.toml` exists but could not be used; the app runs on defaults
    /// and leaves the file alone.
    SettingsUnreadable {
        detail: String,
    },
    /// The library file was not a readable database. It was moved to
    /// `backup` and a fresh one created.
    LibraryReset {
        backup: PathBuf,
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
    /// The standing viewing's countdown to a recorded episode and what came
    /// of it, so the shell can show both. `None` when nothing is being
    /// watched.
    pub watch_progress: Option<WatchProgress>,
    pub settings: Settings,
    /// Oldest first; the shell shows the front one.
    pub notices: Vec<Notice>,
}

impl Default for AppState {
    fn default() -> Self {
        AppState {
            page: Page::Library,
            detail: None,
            library: Vec::new(),
            now_playing: NowPlaying::Idle,
            last_match: None,
            watch_progress: None,
            settings: Settings::default(),
            notices: Vec::new(),
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
    use super::*;

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
    }
}
