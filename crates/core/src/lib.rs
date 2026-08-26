//! Platform-independent application state for Ryuuji.
//!
//! This crate holds the data the shells render. It has no I/O and no
//! platform dependencies, so it compiles everywhere the future Linux shell
//! will. Detection, parsing and persistence arrive in later Foundation
//! issues; for now the state is exactly what the first window needs.

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

/// One tracked show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LibraryEntry {
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

/// The whole application state as the shell sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppState {
    pub page: Page,
    pub library: Vec<LibraryEntry>,
    pub now_playing: NowPlaying,
}

impl Default for AppState {
    fn default() -> Self {
        AppState {
            page: Page::Library,
            library: Vec::new(),
            now_playing: NowPlaying::Idle,
        }
    }
}

impl AppState {
    /// Switches to the page with the given tag. Returns `false` (and leaves
    /// the state untouched) when the tag is unknown.
    pub fn select_page(&mut self, tag: &str) -> bool {
        match Page::from_tag(tag) {
            Some(page) => {
                self.page = page;
                true
            }
            None => false,
        }
    }
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
    fn default_state_is_empty_library_on_library_page() {
        let state = AppState::default();
        assert_eq!(state.page, Page::Library);
        assert!(state.library.is_empty());
        assert_eq!(state.now_playing, NowPlaying::Idle);
    }

    #[test]
    fn select_page_switches_on_known_tag_only() {
        let mut state = AppState::default();
        assert!(state.select_page("now-playing"));
        assert_eq!(state.page, Page::NowPlaying);
        assert!(!state.select_page("unknown"));
        assert_eq!(state.page, Page::NowPlaying);
    }
}
