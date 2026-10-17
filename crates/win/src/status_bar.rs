//! The strip along the bottom of every page: sync in one line, where the
//! playing viewing stands against its recording, with the way to skip it,
//! where the player is in the episode, and while nothing plays, whether
//! tracking is on. A service is named only when it needs the person.

use std::rc::Rc;
use std::time::{Duration, SystemTime};

use ryuuji_core::{AppState, Command, NeedsYou, NowPlaying, SyncLine};
use ryuuji_sync::{Pause, ServiceFacts, SyncError, Syncer};
use windows_reactor::*;

use crate::standing::{Standing, Tone, standing};
use crate::ui::{self, ICON_FONT, clock_text, duration_text};

const HEIGHT: f64 = 32.0;
const CANCEL_GLYPH: &str = "\u{E711}";
const SYNC_GLYPH: &str = "\u{E895}";
const CAUTION_GLYPH: &str = "\u{E7BA}";

/// How often the bar rereads the worker while sync is busy, so a countdown
/// moves and a pause shows.
const TICK: Duration = Duration::from_secs(1);

/// What the bar says, left to right.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct StatusBar {
    /// Sync in one line, from the core; the worker's facts fill in the
    /// seconds and whether a list is out of reach.
    pub sync: Option<SyncLine>,
    /// How many accounts are signed in, which decides "list" or "lists".
    pub lists: usize,
    /// Where the playing viewing stands against its recording.
    pub recording: Option<Standing>,
    /// `11:33 / 24:00` while something plays.
    pub position: Option<String>,
    /// Said while nothing plays, when the bar would otherwise be empty.
    pub tracking: Option<&'static str>,
}

pub(crate) fn status_bar(state: &AppState, detection_down: bool) -> StatusBar {
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
        sync: state.sync_line(),
        lists: state.sync.signed_in_accounts().count(),
        recording: standing(state),
        position,
        tracking,
    }
}

/// Sync's segment: the words, and whether they are a warning.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SyncSegment {
    pub text: String,
    pub warning: bool,
}

/// What sync's segment says, the worker's facts read at `now`. A list out
/// of reach is said over anything short of a service needing the person.
pub(crate) fn sync_segment(
    line: Option<SyncLine>,
    lists: usize,
    facts: &[ServiceFacts],
    now: SystemTime,
) -> Option<SyncSegment> {
    let line = line?;
    let quiet = |text: String| {
        Some(SyncSegment {
            text,
            warning: false,
        })
    };
    let warn = |service: ryuuji_core::Service, what: &str| {
        Some(SyncSegment {
            text: format!("{} \u{b7} {what}", service.label()),
            warning: true,
        })
    };
    if !matches!(line, SyncLine::NeedsYou { .. })
        && let Some(offline) = facts.iter().find(|facts| {
            facts
                .paused
                .is_some_and(|paused| paused.why == Pause::Offline)
        })
    {
        return warn(offline.service, "offline, retrying");
    }
    match line {
        SyncLine::Synced => quiet("Synced".to_owned()),
        SyncLine::Finding => quiet("Finding your shows".to_owned()),
        SyncLine::Sending => {
            let next = facts.iter().filter_map(|facts| facts.next_send).min();
            quiet(match next.and_then(|at| at.duration_since(now).ok()) {
                Some(left) => format!("Sending in {} s", left.as_secs().max(1)),
                None => "Sending".to_owned(),
            })
        }
        SyncLine::Checking => quiet(if lists > 1 {
            "Checking your lists".to_owned()
        } else {
            "Checking your list".to_owned()
        }),
        SyncLine::NeedsYou { service, why } => warn(
            service,
            match why {
                NeedsYou::SignIn => "sign in again",
                NeedsYou::Pick => "pick the entry",
                NeedsYou::PickAnother => "pick another",
                NeedsYou::Rejected => "change refused",
            },
        ),
    }
}

/// What the bar is handed. The worker is compared by identity, as History
/// compares its core handle.
#[derive(Clone)]
pub(crate) struct StatusBarProps {
    pub dispatch: Dispatch<Command>,
    pub bar: StatusBar,
    pub syncer: Rc<std::result::Result<Syncer, SyncError>>,
}

impl PartialEq for StatusBarProps {
    fn eq(&self, other: &StatusBarProps) -> bool {
        self.dispatch == other.dispatch
            && self.bar == other.bar
            && Rc::ptr_eq(&self.syncer, &other.syncer)
    }
}

/// The bar for the bottom row of the chrome. A component, so its countdown
/// redraws the bar alone rather than the window.
pub(crate) fn view(props: &StatusBarProps, cx: &mut RenderCx) -> Element {
    let facts = match &*props.syncer {
        Ok(syncer) => syncer.facts(),
        Err(_) => Vec::new(),
    };
    // The worker's facts change with no dispatch: a countdown's seconds,
    // and a pause starting or ending while there is work.
    let busy = matches!(
        props.bar.sync,
        Some(SyncLine::Finding | SyncLine::Sending | SyncLine::Checking)
    ) || facts.iter().any(|facts| facts.paused.is_some());
    ui::use_refresh_every(cx, busy, TICK);
    let sync = sync_segment(props.bar.sync, props.bar.lists, &facts, SystemTime::now());
    bar(&props.bar, sync.as_ref(), props.dispatch.clone()).into()
}

fn bar(bar: &StatusBar, sync: Option<&SyncSegment>, dispatch: Dispatch<Command>) -> Border {
    let mut segments: Vec<Element> = Vec::new();
    if let Some(sync) = sync {
        segments.push(sync_view(sync));
    }
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

fn recording_segment(standing: &Standing, dispatch: Dispatch<Command>) -> Element {
    let brush = match standing.tone() {
        Tone::Accent => ThemeRef::Accent,
        Tone::Success => ThemeRef::SystemSuccess,
        // The bar keeps caution for sync; a declined episode is said in
        // words here and in caution on Now playing.
        Tone::Neutral | Tone::Caution => ThemeRef::custom(ui::STRONG_FILL),
    };
    let mut parts: Vec<Element> = vec![
        border(Element::Empty)
            .width(8.0)
            .height(8.0)
            .corner_radius(4.0)
            .background(brush)
            .vertical_alignment(VerticalAlignment::Center)
            .into(),
        small(standing.status())
            .foreground(ThemeRef::PrimaryText)
            .vertical_alignment(VerticalAlignment::Center)
            .into(),
    ];
    if let Some(label) = standing.skip_label() {
        parts.push(
            button("")
                .icon(Icon::font(CANCEL_GLYPH))
                .subtle()
                .tooltip(label.clone())
                .automation_name(label.clone())
                .on_click(ui::send(&dispatch, Command::DontRecord))
                .padding(Thickness::uniform(4.0))
                .vertical_alignment(VerticalAlignment::Center)
                .into(),
        );
    }
    hstack(parts).spacing(6.0).into()
}

fn sync_view(sync: &SyncSegment) -> Element {
    let (glyph, brush) = if sync.warning {
        (CAUTION_GLYPH, ThemeRef::SystemCaution)
    } else {
        (SYNC_GLYPH, ThemeRef::SecondaryText)
    };
    hstack((
        text_block(glyph)
            .font_family(ICON_FONT)
            .font_size(12.0)
            .foreground(brush)
            .vertical_alignment(VerticalAlignment::Center),
        small(sync.text.clone()).foreground(ThemeRef::PrimaryText),
    ))
    .spacing(6.0)
    .into()
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

    /// What the bar says of the recording: its tone, its words and its
    /// skip button.
    fn recording(bar: &StatusBar) -> Option<(Tone, String, Option<String>)> {
        bar.recording
            .as_ref()
            .map(|standing| (standing.tone(), standing.status(), standing.skip_label()))
    }

    #[test]
    fn while_something_plays_the_bar_says_the_recording_and_the_position() {
        let bar = status_bar(&playing(RecordOutcome::Counting), false);
        assert_eq!(
            recording(&bar),
            Some((
                Tone::Neutral,
                "Not recording \u{b7} no match".to_owned(),
                None
            ))
        );
        assert_eq!(bar.position.as_deref(), Some("11:33 / 24:00"));
        assert_eq!((bar.sync, bar.lists, bar.tracking), (None, 0, None));
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
            recording(&status_bar(&state, false)),
            Some((
                Tone::Accent,
                "Records in 7:00".to_owned(),
                Some("Don't record episode 1".to_owned())
            ))
        );
    }

    fn facts(next_send: Option<SystemTime>, offline: bool) -> ServiceFacts {
        ServiceFacts {
            service: ryuuji_core::Service::AniList,
            stopped: None,
            paused: offline.then(|| ryuuji_sync::Paused {
                why: Pause::Offline,
                until: SystemTime::UNIX_EPOCH + Duration::from_secs(90),
            }),
            rate_remaining: None,
            last_call: None,
            due: 0,
            waiting: 1,
            running: 0,
            next_send,
        }
    }

    fn said(segment: Option<SyncSegment>) -> Option<(String, bool)> {
        segment.map(|segment| (segment.text, segment.warning))
    }

    #[test]
    fn sync_says_itself_in_a_few_words_and_names_a_service_only_when_it_needs_you() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let quiet = |text: &str| Some((text.to_owned(), false));
        let warning = |text: &str| Some((text.to_owned(), true));
        let at = |secs| SystemTime::UNIX_EPOCH + Duration::from_secs(secs);
        let say = |line, lists, facts: &[ServiceFacts]| said(sync_segment(line, lists, facts, now));
        assert_eq!(say(None, 0, &[]), None);
        assert_eq!(say(Some(SyncLine::Synced), 1, &[]), quiet("Synced"));
        assert_eq!(
            say(Some(SyncLine::Finding), 1, &[]),
            quiet("Finding your shows")
        );
        assert_eq!(
            say(Some(SyncLine::Sending), 1, &[facts(Some(at(1_009)), false)]),
            quiet("Sending in 9 s")
        );
        assert_eq!(
            say(Some(SyncLine::Sending), 1, &[facts(Some(at(900)), false)]),
            quiet("Sending")
        );
        assert_eq!(
            say(Some(SyncLine::Checking), 1, &[]),
            quiet("Checking your list")
        );
        assert_eq!(
            say(Some(SyncLine::Checking), 2, &[]),
            quiet("Checking your lists")
        );
        let needs = |why| {
            Some(SyncLine::NeedsYou {
                service: ryuuji_core::Service::AniList,
                why,
            })
        };
        assert_eq!(
            say(needs(NeedsYou::Pick), 1, &[]),
            warning("AniList \u{b7} pick the entry")
        );
        assert_eq!(
            say(needs(NeedsYou::PickAnother), 1, &[]),
            warning("AniList \u{b7} pick another")
        );
        assert_eq!(
            say(needs(NeedsYou::SignIn), 1, &[]),
            warning("AniList \u{b7} sign in again")
        );
        assert_eq!(
            say(needs(NeedsYou::Rejected), 1, &[]),
            warning("AniList \u{b7} change refused")
        );
        // Out of reach is said over what is under way, and under what needs
        // the person.
        assert_eq!(
            say(Some(SyncLine::Sending), 1, &[facts(None, true)]),
            warning("AniList \u{b7} offline, retrying")
        );
        assert_eq!(
            say(needs(NeedsYou::Pick), 1, &[facts(None, true)]),
            warning("AniList \u{b7} pick the entry")
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
