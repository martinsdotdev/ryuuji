//! Player detection for Ryuuji, modeled on anisthesia: a table of known
//! players plus one strategy per platform, each producing the same
//! normalized [`ryuuji_core::PlaybackEvent`]. The strategies so far are
//! Windows SMTC (`Windows.Media.Control`) and, as signatures only, Linux
//! MPRIS; the table, the session logic and the worker are portable.

#[cfg(target_os = "linux")]
mod mpris;
// The MPRIS stub constructs nothing yet, so on Linux the portable half is
// compiled and tested but not called; the gates go when RYU-63 lands.
#[cfg_attr(not(windows), allow(dead_code))]
mod players;
#[cfg_attr(not(windows), allow(dead_code))]
mod session;
#[cfg(windows)]
mod smtc;
#[cfg_attr(not(windows), allow(dead_code))]
mod watch;

#[cfg(not(any(windows, target_os = "linux")))]
compile_error!(
    "ryuuji-detect has no detection strategy for this target; add one beside smtc.rs and mpris.rs"
);

pub use watch::{WatchError, Watcher, watch};

/// One media session as the strategy last saw it, matched to the player
/// table or not. Diagnostics lists every row, which is how a new table
/// entry is found, so a strategy reports the sessions it cannot use as well
/// as the one it can.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionFacts {
    /// The platform's own id for the session's source: an SMTC source app
    /// user model id, an MPRIS bus name.
    pub app_id: String,
    /// Empty unless the session matched the table and its media properties
    /// could be read.
    pub title: String,
    /// The platform's own word for the state, not a normalized one, so a
    /// value the strategy did not recognise still reads as itself.
    pub status: String,
    /// The table name, or `None` when nothing claimed the id.
    pub player: Option<String>,
}
