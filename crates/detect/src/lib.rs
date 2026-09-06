//! Player detection for Ryuuji, modeled on anisthesia: a table of known
//! players plus per-player strategies, each producing one normalized
//! [`ryuuji_core::PlaybackEvent`]. The only strategy so far is Windows SMTC
//! (`Windows.Media.Control`); the table and the session logic are portable.

#[cfg(windows)]
mod foreground;
#[cfg_attr(not(windows), allow(dead_code))]
mod players;
#[cfg_attr(not(windows), allow(dead_code))]
mod session;
#[cfg(windows)]
mod smtc;

#[cfg(windows)]
pub use smtc::{WatchError, Watcher, watch};

/// Every SMTC session seen at the last refresh, matched or not. `title` is
/// empty for unmatched sessions, whose media properties are not read, and
/// for matched sessions whose media properties were not ready.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionFacts {
    pub app_id: String,
    pub title: String,
    pub status: String,
    pub player: Option<String>,
}
