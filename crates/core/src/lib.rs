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
mod matching;
mod playback;
mod settings;
mod store;

use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

pub use app::Ryuuji;
pub use data_dir::{DataDir, DataDirError, DataDirSource};
pub use diagnostics::{ByteSize, Diagnostics, FileFacts, FileStat, ProbeFailed};
pub use matching::{Confidence, ProposedMatch, normalize_title, propose, similarity};
pub use playback::{PlaybackEvent, PlaybackSource, PlaybackStatus};
// Shells depend on this crate alone, so the parser reaches them through here.
pub use ryuuji_parse::{ElementKind, Options, parse};
pub use store::{DbError, Opened, Recovered, SchemaVersion, Store, StoreError};

/// Every destination in the navigation pane, so the variant list is the pane
/// list. A view reached from a page rather than from the pane is a [`Detail`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    Library,
    NowPlaying,
    Settings,
}

impl Page {
    /// The navigation pane items, in order.
    pub const ALL: [Page; 3] = [Page::Library, Page::NowPlaying, Page::Settings];

    /// Stable identifier used as the navigation item tag.
    pub fn tag(self) -> &'static str {
        match self {
            Page::Library => "library",
            Page::NowPlaying => "now-playing",
            Page::Settings => "settings",
        }
    }

    /// Inverse of [`Page::tag`].
    pub fn from_tag(tag: &str) -> Option<Page> {
        Page::ALL.into_iter().find(|page| page.tag() == tag)
    }

    /// Human-readable label.
    pub fn label(self) -> &'static str {
        match self {
            Page::Library => "Library",
            Page::NowPlaying => "Now playing",
            Page::Settings => "Settings",
        }
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

/// Where a show sits in the user's library.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WatchStatus {
    Watching,
    Completed,
    OnHold,
    Dropped,
    PlanToWatch,
}

impl WatchStatus {
    /// Every status, in the order the UI lists them.
    pub const ALL: [WatchStatus; 5] = [
        WatchStatus::Watching,
        WatchStatus::Completed,
        WatchStatus::OnHold,
        WatchStatus::Dropped,
        WatchStatus::PlanToWatch,
    ];

    /// Stable identifier stored in the library database.
    pub fn tag(self) -> &'static str {
        match self {
            WatchStatus::Watching => "watching",
            WatchStatus::Completed => "completed",
            WatchStatus::OnHold => "on-hold",
            WatchStatus::Dropped => "dropped",
            WatchStatus::PlanToWatch => "plan-to-watch",
        }
    }

    /// Inverse of [`WatchStatus::tag`].
    pub fn from_tag(tag: &str) -> Option<WatchStatus> {
        WatchStatus::ALL
            .into_iter()
            .find(|status| status.tag() == tag)
    }

    pub fn label(self) -> &'static str {
        match self {
            WatchStatus::Watching => "Watching",
            WatchStatus::Completed => "Completed",
            WatchStatus::OnHold => "On hold",
            WatchStatus::Dropped => "Dropped",
            WatchStatus::PlanToWatch => "Plan to watch",
        }
    }
}

/// Which colour theme the shell should request from the platform.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ThemePreference {
    /// Follow the operating system setting.
    #[default]
    System,
    Light,
    Dark,
}

impl ThemePreference {
    /// Every preference, in the order the UI lists them.
    pub const ALL: [ThemePreference; 3] = [
        ThemePreference::System,
        ThemePreference::Light,
        ThemePreference::Dark,
    ];

    /// Stable identifier written to `settings.toml`.
    pub fn tag(self) -> &'static str {
        match self {
            ThemePreference::System => "system",
            ThemePreference::Light => "light",
            ThemePreference::Dark => "dark",
        }
    }

    /// Inverse of [`ThemePreference::tag`].
    pub fn from_tag(tag: &str) -> Option<ThemePreference> {
        ThemePreference::ALL
            .into_iter()
            .find(|theme| theme.tag() == tag)
    }

    pub fn label(self) -> &'static str {
        match self {
            ThemePreference::System => "System",
            ThemePreference::Light => "Light",
            ThemePreference::Dark => "Dark",
        }
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
    /// Creates a Watching entry from the unmatched last proposal and relinks it.
    AddProposedToLibrary,
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
    fn page_tags_round_trip() {
        for page in Page::ALL {
            assert_eq!(Page::from_tag(page.tag()), Some(page));
        }
        assert_eq!(Page::from_tag("nope"), None);
    }

    #[test]
    fn watch_status_tags_round_trip() {
        for status in WatchStatus::ALL {
            assert_eq!(WatchStatus::from_tag(status.tag()), Some(status));
        }
        assert_eq!(WatchStatus::from_tag("nope"), None);
    }

    #[test]
    fn theme_tags_round_trip() {
        for theme in ThemePreference::ALL {
            assert_eq!(ThemePreference::from_tag(theme.tag()), Some(theme));
        }
        assert_eq!(ThemePreference::from_tag("blue"), None);
    }

    #[test]
    fn playback_status_labels() {
        assert_eq!(PlaybackStatus::Playing.label(), "Playing");
        assert_eq!(PlaybackStatus::Paused.label(), "Paused");
        assert_eq!(PlaybackStatus::Stopped.label(), "Stopped");
    }

    #[test]
    fn default_state_is_empty_library_on_library_page() {
        let state = AppState::default();
        assert_eq!(state.page, Page::Library);
        assert_eq!(state.detail, None);
        assert!(state.library.is_empty());
        assert_eq!(state.now_playing, NowPlaying::Idle);
        assert_eq!(state.last_match, None);
        assert_eq!(state.settings.theme, ThemePreference::System);
        assert!(state.notices.is_empty());
    }
}
