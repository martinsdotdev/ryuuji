//! What a player looks like once a detection strategy has finished with it.
//! Strategies and the inject control both hand the core a [`PlaybackEvent`];
//! the core never sees window titles, process names or any other raw
//! platform field.

use std::time::{Duration, SystemTime};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaybackStatus {
    Playing,
    Paused,
    Stopped,
}

impl PlaybackStatus {
    pub fn label(self) -> &'static str {
        match self {
            PlaybackStatus::Playing => "Playing",
            PlaybackStatus::Paused => "Paused",
            PlaybackStatus::Stopped => "Stopped",
        }
    }
}

/// Where an observation came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaybackSource {
    /// A detection strategy watching a real player.
    Detected,
    /// The inject control on the Diagnostics page.
    Injected,
}

impl PlaybackSource {
    /// Stable identifier used in log events.
    pub fn tag(self) -> &'static str {
        match self {
            PlaybackSource::Detected => "detected",
            PlaybackSource::Injected => "injected",
        }
    }
}

/// One normalized observation of a player. Never carries raw platform fields.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlaybackEvent {
    /// The player table name, or `"Injected"`.
    pub player: String,
    /// Verbatim from the player; may be empty.
    pub title: String,
    pub status: PlaybackStatus,
    /// Saturating, from the start of the item.
    pub position: Duration,
    /// [`Duration::ZERO`] means unknown.
    pub duration: Duration,
    pub observed_at: SystemTime,
    pub source: PlaybackSource,
    /// Whether the player's window is in front. `None` when the platform
    /// cannot tell; the accrual rule treats that as watchable, so a source
    /// without a focus signal still records.
    pub foreground: Option<bool>,
}
