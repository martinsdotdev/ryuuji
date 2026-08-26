//! Platform-independent application state for Ryuuji.
//!
//! This crate owns the data the shells render and the store that persists
//! it. Shells talk to it through [`Ryuuji`]: open it once, read
//! [`Ryuuji::state`], and feed user actions through [`Ryuuji::dispatch`].
//! Nothing platform-specific lives here, so it compiles everywhere the
//! future Linux shell will.

mod app;
mod data_dir;
mod store;

use std::fmt;

pub use app::Ryuuji;
pub use data_dir::{DataDir, DataDirError};
pub use store::{DbError, Store, StoreError};

/// Top-level pages reachable from the navigation pane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    Library,
    NowPlaying,
    Settings,
}

impl Page {
    /// Every page, in navigation order.
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

    /// Human-readable navigation label.
    pub fn label(self) -> &'static str {
        match self {
            Page::Library => "Library",
            Page::NowPlaying => "Now playing",
            Page::Settings => "Settings",
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
    /// Something identifiable is playing.
    Playing {
        title: String,
        episode: u32,
        player: String,
    },
}

/// Everything a shell can ask the core to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    SelectPage(Page),
    AddEntry(NewEntry),
    SetProgress { id: EntryId, progress: u32 },
    SetStatus { id: EntryId, status: WatchStatus },
    DismissNotice,
}

/// Something the shell should surface to the user until dismissed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Notice {
    SaveFailed { detail: String },
}

/// The whole application state as the shell sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppState {
    pub page: Page,
    pub library: Vec<LibraryEntry>,
    pub now_playing: NowPlaying,
    pub notice: Option<Notice>,
}

impl Default for AppState {
    fn default() -> Self {
        AppState {
            page: Page::Library,
            library: Vec::new(),
            now_playing: NowPlaying::Idle,
            notice: None,
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
    fn default_state_is_empty_library_on_library_page() {
        let state = AppState::default();
        assert_eq!(state.page, Page::Library);
        assert!(state.library.is_empty());
        assert_eq!(state.now_playing, NowPlaying::Idle);
        assert_eq!(state.notice, None);
    }
}
