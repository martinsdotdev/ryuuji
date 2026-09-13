//! The Linux MPRIS strategy: bus names under `org.mpris.MediaPlayer2.`,
//! `PlaybackStatus`, `Metadata` with `xesam:title` and `mpris:length`, and
//! `Position` in microseconds. `NameOwnerChanged` on one of those names
//! posts [`Waker::sessions_changed`]; `PropertiesChanged` and `Seeked` post
//! [`Waker::session_changed`]; `Position` never signals progress, so it is
//! re-read on the worker's poll while something plays.
//!
//! Not implemented (RYU-63). The four items are here so the shared worker
//! compiles on Linux and the ubuntu job proves the seam holds.

use std::time::SystemTime;

use crate::watch::{Refresh, Wake, Waker};

pub(crate) const NAME: &str = "MPRIS";

#[derive(Debug, thiserror::Error)]
pub(crate) enum StartError {
    /// What a Linux caller of `watch` gets until RYU-63: an error it can
    /// show, not a panic on the worker thread.
    #[error("the MPRIS strategy is not implemented yet (RYU-63)")]
    Unimplemented,
}

/// Will own the session bus connection and the player table; a unit struct
/// until RYU-63 fills it in.
pub(crate) struct Source;

impl Source {
    pub(crate) fn start(_waker: Waker) -> Result<Source, StartError> {
        Err(StartError::Unimplemented)
    }

    /// Every event will carry `foreground: None`: the front window is the
    /// compositor's to know and Wayland has no portable answer, so
    /// `Front::Unknown` is the honest reading here.
    pub(crate) fn refresh(&mut self, _wake: Wake, _now: SystemTime) -> Refresh<'_> {
        todo!("RYU-63: list the org.mpris.MediaPlayer2.* owners and read each one")
    }
}
