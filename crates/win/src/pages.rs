//! Page bodies. The Library lists real entries, Settings is laid out in the
//! Windows Settings idiom (section headers and cards) with the theme picker,
//! the data folder and the way into Diagnostics, and Now playing shows the
//! last playback observation on a card, or a placeholder in the symbolic
//! empty-state style the UI-direction research settled on (heading required,
//! neutral tone). An open detail is built by the caller and handed in, so
//! this module never sees the core handle or the log buffer.

use std::time::{Duration, SystemTime};

use ryuuji_core::{
    AppState, Command, DataDir, Detail, LibraryEntry, Notice, NowPlaying, Options, Page,
    ProposedMatch, Settings, StoreError, ThemePreference, error_chain, parse,
};
use windows_reactor::*;

use crate::ui::{
    self, APP_VERSION, BUILD_PROFILE, CONTENT_MAX_WIDTH, FOLDER_GLYPH, REPAIR_GLYPH, age_of,
    caption, card, card_frame, enum_picker, section, table,
};

const PAGE_PADDING: f64 = 24.0;

/// Renders the notice bar and the body: the open `detail`, or the currently
/// selected page when nothing is open over it.
pub fn render(
    state: &AppState,
    dispatch: Dispatch<Command>,
    dir: &DataDir,
    detection_down: bool,
    detail: Option<Element>,
) -> Element {
    let page: Element = match detail {
        Some(detail) => detail,
        None => match state.page {
            Page::Library => library(&state.library),
            Page::NowPlaying => component(
                now_playing,
                NowPlayingProps {
                    now_playing: state.now_playing.clone(),
                    last_match: state.last_match.clone(),
                    library: state.library.clone(),
                    detection_down,
                    dispatch: dispatch.clone(),
                },
            ),
            Page::Settings => component(
                settings,
                SettingsProps {
                    settings: state.settings.clone(),
                    dir: dir.clone(),
                    dispatch: dispatch.clone(),
                },
            ),
        },
    };

    grid((
        notice(&state.notices, dispatch),
        border(page)
            .padding(Thickness::uniform(PAGE_PADDING))
            .grid_row(1),
    ))
    .rows([GridLength::Auto, GridLength::STAR])
    .into()
}

/// Shown instead of the shell when the library could not be opened.
pub fn boot_failed(err: &StoreError, dir: &DataDir) -> Element {
    border(placeholder(
        "Ryuuji couldn't open its library",
        format!(
            "{}\nData directory: {}",
            error_chain(err),
            dir.root().display()
        ),
    ))
    .padding(Thickness::uniform(PAGE_PADDING))
    .into()
}

/// Shows the oldest notice, or nothing at all when the queue is empty.
/// Keyed on the queue length so a dismissal remounts the bar: `IsOpen` is
/// diffed, and WinUI has already closed it. That key only distinguishes one
/// notice from the next because the queue is FIFO and only ever loses its
/// head, so a push and a pop cannot land on the same length.
fn notice(notices: &[Notice], dispatch: Dispatch<Command>) -> Element {
    let (severity, title, message) = match notices.first() {
        Some(Notice::SaveFailed { detail }) => (
            InfoBarSeverity::Error,
            "Couldn't save your change",
            detail.clone(),
        ),
        Some(Notice::SettingsUnreadable { detail }) => (
            InfoBarSeverity::Warning,
            "settings.toml could not be read",
            detail.clone(),
        ),
        Some(Notice::LibraryReset { backup }) => (
            InfoBarSeverity::Warning,
            "Your library was reset",
            format!(
                "The previous file wasn't a readable database. It was kept at {}.",
                backup.display()
            ),
        ),
        None => return Element::Empty,
    };
    InfoBar::new(title)
        .message(message)
        .severity(severity)
        .is_open(true)
        .on_closed(move || dispatch.call(Command::DismissNotice))
        .with_key(notices.len().to_string())
        .grid_row(0)
        .into()
}

fn library(entries: &[LibraryEntry]) -> Element {
    if entries.is_empty() {
        return placeholder(
            "Your library is empty",
            "Shows you track will appear here. Add one from Now playing when an episode is detected.",
        );
    }
    list_view(entries.to_vec(), |entry, _| {
        vstack((
            text_block(entry.title.clone()).semibold(),
            text_block(status_line(entry)).foreground(ThemeRef::SecondaryText),
        ))
        .spacing(2.0)
    })
    .with_key_selector(|entry| entry.id.to_string())
    .into()
}

fn status_line(entry: &LibraryEntry) -> String {
    match entry.total {
        Some(total) => format!("{} · {}/{}", entry.status.label(), entry.progress, total),
        None => format!("{} · {}", entry.status.label(), entry.progress),
    }
}

/// What Now playing shows. A standing proposal is captioned with its age,
/// so while nothing is playing the page runs a clock to keep that current.
#[derive(Clone, Debug, PartialEq, Eq)]
struct NowPlayingProps {
    now_playing: NowPlaying,
    last_match: Option<ProposedMatch>,
    library: Vec<LibraryEntry>,
    detection_down: bool,
    dispatch: Dispatch<Command>,
}

fn now_playing(props: &NowPlayingProps, cx: &mut RenderCx) -> Element {
    let idle = matches!(props.now_playing, NowPlaying::Idle);
    ui::use_refresh(cx, idle && props.last_match.is_some());

    let now_playing = &props.now_playing;
    let library = &props.library;
    let now = SystemTime::now();
    let top: Element = match now_playing {
        NowPlaying::Idle => {
            let (heading, body) = idle_placeholder_copy(props.detection_down);
            placeholder(heading, body)
        }
        NowPlaying::Detecting => placeholder(
            "Detecting…",
            "A player is open; waiting for something recognisable.",
        ),
        NowPlaying::Playing {
            title,
            player,
            status,
            position,
            duration,
        } => card_frame(
            vstack((
                text_block(title.clone()).font_size(20.0).semibold().wrap(),
                caption(format!("{} in {player}", status.label())),
                text_block(format!(
                    "{} / {}",
                    clock_text(*position),
                    duration_text(*duration)
                )),
            ))
            .spacing(4.0),
        )
        .into(),
    };
    match &props.last_match {
        Some(m) => vstack((
            top,
            proposal_card(m, library, idle, now, props.dispatch.clone()),
        ))
        .spacing(8.0)
        .into(),
        None => top,
    }
}

/// The Now playing placeholder copy: the plain idle pair, or the
/// detection-down pair when the watcher never started.
fn idle_placeholder_copy(detection_down: bool) -> (&'static str, &'static str) {
    if detection_down {
        (
            "Detection isn't running",
            "Ryuuji can't watch your players right now. See Diagnostics in Settings for what went wrong.",
        )
    } else {
        (
            "Nothing playing",
            "Open an episode in your player and it will show up here.",
        )
    }
}

fn proposal_card(
    m: &ProposedMatch,
    library: &[LibraryEntry],
    idle: bool,
    now: SystemTime,
    dispatch: Dispatch<Command>,
) -> Element {
    let title = text_block(m.title_in(library))
        .font_size(20.0)
        .semibold()
        .wrap();
    let mut children: Vec<Element> =
        vec![title.into(), caption(match_caption(m, idle, now)).into()];
    let rows = fact_rows(m);
    if !rows.is_empty() {
        children.push(facts_grid(&rows));
    }
    if !m.link.names_entry() {
        children.push(
            button("Add to library")
                .on_click(move || dispatch.call(Command::AddProposedToLibrary))
                .horizontal_alignment(HorizontalAlignment::Left)
                .into(),
        );
    }
    card_frame(vstack(children).spacing(4.0)).into()
}

fn match_caption(m: &ProposedMatch, idle: bool, now: SystemTime) -> String {
    let mut parts = Vec::new();
    if let Some(episode) = m.episode {
        parts.push(format!("Episode {episode}"));
    }
    parts.push(m.link.confidence().label().to_owned());
    if idle {
        parts.push(format!("Last seen in {}", m.player));
        parts.push(age_of(m.at, now));
    }
    parts.join(" \u{b7} ")
}

/// Every element the parser finds in the raw player title. Re-parsed here
/// rather than carried on the proposal, so a reloaded one reads the same.
fn fact_rows(m: &ProposedMatch) -> Vec<(String, String)> {
    parse(&m.raw_title, &Options::default())
        .iter()
        .map(|(kind, value)| (fact_label(kind.label()), value.to_owned()))
        .collect()
}

fn fact_label(label: &str) -> String {
    let mut label = label.replace('_', " ");
    if let Some(first) = label.get_mut(..1) {
        first.make_ascii_uppercase();
    }
    label
}

fn facts_grid(rows: &[(String, String)]) -> Element {
    table([GridLength::Auto, GridLength::STAR])
        .spacing(6.0, 16.0)
        .rows(
            rows.iter()
                .map(|(label, value)| [caption(label.clone()), text_block(value.clone()).wrap()]),
        )
        .into()
}

/// `m:ss`, or `h:mm:ss` from one hour.
fn clock_text(value: Duration) -> String {
    let total = value.as_secs();
    let (hours, minutes, seconds) = (total / 3600, total / 60 % 60, total % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

/// Like [`clock_text`], with an unknown length shown as `--:--`.
fn duration_text(value: Duration) -> String {
    if value.is_zero() {
        "--:--".to_string()
    } else {
        clock_text(value)
    }
}

/// What Settings shows. The outcome of the last folder action is the page's
/// own, so it lives in the page rather than in the shell.
#[derive(Clone, Debug, PartialEq, Eq)]
struct SettingsProps {
    settings: Settings,
    dir: DataDir,
    dispatch: Dispatch<Command>,
}

fn settings(props: &SettingsProps, cx: &mut RenderCx) -> Element {
    let (folder_action, set_folder_action) = cx.use_state(None::<String>);

    let settings = &props.settings;
    let dir = &props.dir;
    let dispatch = props.dispatch.clone();
    let root = dir.root().to_path_buf();
    let open_folder = move || set_folder_action.call(Some(ui::open_folder(&root)));
    let open_diagnostics = {
        let dispatch = dispatch.clone();
        move || dispatch.call(Command::OpenDetail(Detail::Diagnostics))
    };
    scroll_viewer(
        vstack((
            vstack((section("Appearance"), theme_card(settings.theme, dispatch))).spacing(8.0),
            vstack((
                section("Your data"),
                card(
                    Some(FOLDER_GLYPH),
                    "Data folder",
                    dir.root().display().to_string(),
                    hstack((
                        button("Open folder").on_click(open_folder),
                        caption(folder_action.unwrap_or_default())
                            .vertical_alignment(VerticalAlignment::Center),
                    ))
                    .spacing(12.0)
                    .into(),
                ),
                card(
                    Some(REPAIR_GLYPH),
                    "Diagnostics",
                    "Paths, schema version and recent log events",
                    button("Open diagnostics").on_click(open_diagnostics).into(),
                ),
            ))
            .spacing(8.0),
            caption(format!("Ryuuji {APP_VERSION} · {BUILD_PROFILE} build")),
        ))
        .spacing(24.0)
        .max_width(CONTENT_MAX_WIDTH),
    )
    .into()
}

/// The theme picker, a drop-down like Windows Settings' "Choose your mode".
fn theme_card(theme: ThemePreference, dispatch: Dispatch<Command>) -> Border {
    let combo = enum_picker(
        &ThemePreference::ALL,
        ThemePreference::label,
        theme,
        move |chosen| dispatch.call(Command::SetTheme(chosen)),
    )
    .min_width(160.0);
    card(
        None,
        "Theme",
        "Follow Windows, or pick light or dark.",
        combo.into(),
    )
}

/// Symbolic placeholder: a heading and one line of body text on a card.
fn placeholder(heading: impl Into<String>, body: impl Into<String>) -> Element {
    card_frame(
        vstack((
            text_block(heading).font_size(20.0).semibold(),
            text_block(body).foreground(ThemeRef::SecondaryText).wrap(),
        ))
        .spacing(4.0),
    )
    .into()
}

#[cfg(test)]
mod tests {
    use ryuuji_core::Link;

    use super::*;

    #[test]
    fn clock_text_formats_minutes_and_hours() {
        assert_eq!(clock_text(Duration::ZERO), "0:00");
        assert_eq!(clock_text(Duration::from_secs(305)), "5:05");
        assert_eq!(clock_text(Duration::from_secs(3661)), "1:01:01");
    }

    #[test]
    fn duration_text_marks_zero_unknown() {
        assert_eq!(duration_text(Duration::ZERO), "--:--");
        assert_eq!(duration_text(Duration::from_secs(1420)), "23:40");
    }

    fn proposal() -> ProposedMatch {
        ProposedMatch {
            raw_title: "raw.mkv".to_owned(),
            parsed_title: "Parsed".to_owned(),
            episode: None,
            season: None,
            release_group: None,
            link: Link::Unmatched,
            player: "mpv".to_owned(),
            at: std::time::SystemTime::UNIX_EPOCH,
        }
    }

    #[test]
    fn match_caption_joins_episode_confidence_and_idle_player() {
        let m = ProposedMatch {
            episode: Some(1),
            ..proposal()
        };
        assert_eq!(
            match_caption(&m, true, SystemTime::UNIX_EPOCH),
            "Episode 1 \u{b7} No library entry \u{b7} Last seen in mpv \u{b7} 0 s ago"
        );
        assert_eq!(
            match_caption(&proposal(), false, SystemTime::UNIX_EPOCH),
            "No library entry"
        );
    }

    #[test]
    fn match_caption_appends_last_seen_and_age_when_idle() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(2 * 3600);
        assert_eq!(
            match_caption(&proposal(), true, now),
            "No library entry \u{b7} Last seen in mpv \u{b7} 2 h ago"
        );
        assert_eq!(match_caption(&proposal(), false, now), "No library entry");
    }

    #[test]
    fn idle_placeholder_copy_branches_on_detection_down() {
        assert_eq!(
            idle_placeholder_copy(false),
            (
                "Nothing playing",
                "Open an episode in your player and it will show up here."
            )
        );
        assert_eq!(
            idle_placeholder_copy(true),
            (
                "Detection isn't running",
                "Ryuuji can't watch your players right now. See Diagnostics in Settings for what went wrong."
            )
        );
    }

    #[test]
    fn fact_rows_re_parse_the_raw_title() {
        let m = ProposedMatch {
            raw_title: "[Subs] Show - 03 (1080p).mkv".to_owned(),
            ..proposal()
        };
        let rows = fact_rows(&m);
        assert!(rows.contains(&("Release group".to_owned(), "Subs".to_owned())));
        assert!(rows.contains(&("Anime title".to_owned(), "Show".to_owned())));
        assert!(rows.contains(&("Episode number".to_owned(), "03".to_owned())));
    }

    #[test]
    fn fact_label_humanises_snake_case() {
        assert_eq!(fact_label("release_group"), "Release group");
        assert_eq!(fact_label("anime_title"), "Anime title");
        assert_eq!(fact_label("source"), "Source");
    }
}
