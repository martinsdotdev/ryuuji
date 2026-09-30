//! The strip along the bottom of every page: where the playing viewing
//! stands against its recording, with the way to skip it, where the player
//! is in the episode, and while nothing plays, whether tracking is on.

use ryuuji_core::{AppState, Command, NowPlaying};
use windows_reactor::*;

use crate::standing::{Standing, standing};
use crate::ui::{clock_text, duration_text};

const HEIGHT: f64 = 32.0;
const CANCEL_GLYPH: &str = "\u{E711}";

/// The colour of the dot beside the recording's words.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Dot {
    /// Counting down to the write.
    Accent,
    /// Written.
    Success,
    /// Nothing is being written.
    Neutral,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Recording {
    pub dot: Dot,
    pub text: String,
    /// What the button that skips the viewing says, while there is a
    /// countdown for it to stop.
    pub skip: Option<String>,
}

/// What the bar says, left to right.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct StatusBar {
    pub recording: Option<Recording>,
    /// `11:33 / 24:00` while something plays.
    pub position: Option<String>,
    /// Said while nothing plays, when the bar would otherwise be empty.
    pub tracking: Option<&'static str>,
}

pub(crate) fn status_bar(state: &AppState, detection_down: bool) -> StatusBar {
    let recording = standing(state).map(|standing| Recording {
        dot: match standing {
            Standing::Counting { .. } => Dot::Accent,
            Standing::Recorded { .. } => Dot::Success,
            Standing::Undone { .. } | Standing::NotRecording { .. } => Dot::Neutral,
        },
        text: standing.status(),
        skip: standing.skip_label(),
    });
    let (position, tracking) = match &state.now_playing {
        NowPlaying::Playing {
            position, duration, ..
        } => (
            Some(format!(
                "{} / {}",
                clock_text(*position),
                duration_text(*duration)
            )),
            None,
        ),
        NowPlaying::Idle | NowPlaying::Detecting => (
            None,
            Some(if detection_down {
                "Tracking is off"
            } else {
                "Tracking is on"
            }),
        ),
    };
    StatusBar {
        recording,
        position,
        tracking,
    }
}

/// Builds the bar for the bottom row of the chrome.
pub(crate) fn bar(bar: &StatusBar, dispatch: Dispatch<Command>) -> Border {
    let mut segments: Vec<Element> = Vec::new();
    if let Some(recording) = &bar.recording {
        segments.push(recording_segment(recording, dispatch));
    }
    if let Some(position) = &bar.position {
        segments.push(small(position.clone()).into());
    }
    if let Some(tracking) = bar.tracking {
        segments.push(small(tracking).into());
    }
    let mut items: Vec<Element> = Vec::new();
    for segment in segments {
        if !items.is_empty() {
            items.push(divider());
        }
        items.push(segment);
    }
    border(
        hstack(items)
            .spacing(12.0)
            .horizontal_alignment(HorizontalAlignment::Right)
            .vertical_alignment(VerticalAlignment::Center),
    )
    .height(HEIGHT)
    .padding(Thickness {
        left: 16.0,
        right: 16.0,
        ..Thickness::default()
    })
    .border_thickness(Thickness {
        top: 1.0,
        ..Thickness::default()
    })
    .border_brush(ThemeRef::DividerStroke)
}

fn recording_segment(recording: &Recording, dispatch: Dispatch<Command>) -> Element {
    let brush = match recording.dot {
        Dot::Accent => ThemeRef::Accent,
        Dot::Success => ThemeRef::SystemSuccess,
        Dot::Neutral => ThemeRef::custom("ControlStrongFillColorDefaultBrush"),
    };
    let mut parts: Vec<Element> = vec![
        border(Element::Empty)
            .width(8.0)
            .height(8.0)
            .corner_radius(4.0)
            .background(brush)
            .vertical_alignment(VerticalAlignment::Center)
            .into(),
        small(recording.text.clone())
            .foreground(ThemeRef::PrimaryText)
            .vertical_alignment(VerticalAlignment::Center)
            .into(),
    ];
    if let Some(label) = &recording.skip {
        parts.push(
            button("")
                .icon(Icon::font(CANCEL_GLYPH))
                .subtle()
                .tooltip(label.clone())
                .automation_name(label.clone())
                .on_click(move || dispatch.call(Command::DontRecord))
                .padding(Thickness::uniform(4.0))
                .vertical_alignment(VerticalAlignment::Center)
                .into(),
        );
    }
    hstack(parts).spacing(6.0).into()
}

fn small(text: impl Into<String>) -> TextBlock {
    text_block(text)
        .font_size(12.0)
        .foreground(ThemeRef::SecondaryText)
        .vertical_alignment(VerticalAlignment::Center)
}

fn divider() -> Element {
    border(Element::Empty)
        .width(1.0)
        .height(16.0)
        .background(ThemeRef::DividerStroke)
        .vertical_alignment(VerticalAlignment::Center)
        .into()
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use ryuuji_core::{Link, PlaybackStatus, ProposedMatch, RecordOutcome, WatchProgress};

    use super::*;

    fn playing(outcome: RecordOutcome) -> AppState {
        AppState {
            now_playing: NowPlaying::Playing {
                title: "Show - 01.mkv".to_owned(),
                player: "mpv".to_owned(),
                status: PlaybackStatus::Playing,
                position: Duration::from_secs(693),
                duration: Duration::from_secs(1440),
            },
            last_match: Some(ProposedMatch {
                raw_title: "Show - 01.mkv".to_owned(),
                parsed_title: "Show".to_owned(),
                episode: Some(1..=1),
                season: None,
                release_group: None,
                link: Link::Unmatched,
                player: "mpv".to_owned(),
                at: SystemTime::UNIX_EPOCH,
            }),
            watch_progress: Some(WatchProgress {
                accrued: Duration::from_secs(300),
                threshold: Duration::from_secs(720),
                outcome,
            }),
            ..AppState::default()
        }
    }

    #[test]
    fn while_something_plays_the_bar_says_the_recording_and_the_position() {
        assert_eq!(
            status_bar(&playing(RecordOutcome::Counting), false),
            StatusBar {
                recording: Some(Recording {
                    dot: Dot::Neutral,
                    text: "Not recording \u{b7} no match".to_owned(),
                    skip: None,
                }),
                position: Some("11:33 / 24:00".to_owned()),
                tracking: None,
            }
        );
    }

    #[test]
    fn while_nothing_plays_the_bar_says_whether_tracking_is_on() {
        let idle = AppState::default();
        assert_eq!(
            status_bar(&idle, false),
            StatusBar {
                tracking: Some("Tracking is on"),
                ..StatusBar::default()
            }
        );
        assert_eq!(status_bar(&idle, true).tracking, Some("Tracking is off"));
        // A viewing left standing after the player stopped has nothing to
        // count, so the bar does not say it.
        let stopped = AppState {
            now_playing: NowPlaying::Idle,
            ..playing(RecordOutcome::Counting)
        };
        assert_eq!(status_bar(&stopped, false).recording, None);
    }

    #[test]
    fn a_countdown_gets_the_accent_dot_and_the_skip_button() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = ryuuji_core::DataDir::at(tmp.path()).unwrap();
        let ryuuji_core::Opened { mut store, .. } = ryuuji_core::Store::open(&dir).unwrap();
        let entry = store
            .add(ryuuji_core::NewEntry {
                title: "Show".to_owned(),
                status: ryuuji_core::WatchStatus::Watching,
                progress: 0,
                total: Some(12),
                rewatching: false,
            })
            .unwrap();
        let mut state = playing(RecordOutcome::Counting);
        state.library = vec![entry.clone()];
        if let Some(m) = state.last_match.as_mut() {
            m.link = Link::Exact(entry.id);
        }
        assert_eq!(
            status_bar(&state, false).recording,
            Some(Recording {
                dot: Dot::Accent,
                text: "Records in 7:00".to_owned(),
                skip: Some("Don't record episode 1".to_owned()),
            })
        );
    }

    #[test]
    fn an_unknown_length_reads_as_dashes() {
        let mut state = playing(RecordOutcome::Counting);
        state.now_playing = NowPlaying::Playing {
            title: "Show - 01.mkv".to_owned(),
            player: "mpv".to_owned(),
            status: PlaybackStatus::Playing,
            position: Duration::from_secs(65),
            duration: Duration::ZERO,
        };
        assert_eq!(
            status_bar(&state, false).position.as_deref(),
            Some("1:05 / --:--")
        );
    }
}
