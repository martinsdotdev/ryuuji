//! Page bodies. The Library lists real entries, Settings is laid out in the
//! Windows Settings idiom (section headers and cards) with the theme picker,
//! the data folder and the way into Diagnostics, and Now playing shows the
//! last playback observation on a card, or a placeholder in the symbolic
//! empty-state style the UI-direction research settled on (heading required,
//! neutral tone). The Diagnostics body itself is built by the caller, so this
//! module never sees the core handle or the log buffer.

use std::time::{Duration, SystemTime};

use ryuuji_core::{
    AppState, Command, Confidence, DataDir, LibraryEntry, Notice, NowPlaying, Page, ProposedMatch,
    Settings, StoreError, ThemePreference, error_chain,
};
use windows_reactor::*;

use crate::debug;
use crate::ui::{
    CONTENT_MAX_WIDTH, FOLDER_GLYPH, REPAIR_GLYPH, age_of, caption, card, card_frame, section,
};

const PAGE_PADDING: f64 = 24.0;

/// What the shell threads into the pages beyond the core state: whether the
/// watcher came up, the render instant for relative ages, and the Settings
/// folder-action outcome and its setter.
pub struct Env {
    pub detection_down: bool,
    pub now: SystemTime,
    pub folder_action: Option<String>,
    pub set_folder_action: SetState<Option<String>>,
}

/// Renders the notice bar and the body for the currently selected page.
pub fn render(
    state: &AppState,
    dispatch: Dispatch<Command>,
    dir: &DataDir,
    env: Env,
    debug: impl FnOnce() -> Element,
) -> Element {
    let page: Element = match state.page {
        Page::Library => library(&state.library),
        Page::NowPlaying => now_playing(
            &state.now_playing,
            state.last_match.as_ref(),
            &state.library,
            env.detection_down,
            env.now,
            dispatch.clone(),
        ),
        Page::Settings => settings(
            &state.settings,
            dir,
            dispatch.clone(),
            env.folder_action,
            env.set_folder_action,
        ),
        Page::Debug => debug(),
    };

    grid((
        notice(&state.notices, dispatch).grid_row(0),
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

/// Shows the oldest notice. Keyed on the queue length so a dismissal
/// remounts the bar: `IsOpen` is diffed, and WinUI has already closed it.
fn notice(notices: &[Notice], dispatch: Dispatch<Command>) -> InfoBar {
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
        None => (InfoBarSeverity::Informational, "", String::new()),
    };
    InfoBar::new(title)
        .message(message)
        .severity(severity)
        .is_open(!notices.is_empty())
        .on_closed(move || dispatch.call(Command::DismissNotice))
        .with_key(notices.len().to_string())
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

fn now_playing(
    now_playing: &NowPlaying,
    last_match: Option<&ProposedMatch>,
    library: &[LibraryEntry],
    detection_down: bool,
    now: SystemTime,
    dispatch: Dispatch<Command>,
) -> Element {
    let top: Element = match now_playing {
        NowPlaying::Idle => {
            let (heading, body) = idle_placeholder_copy(detection_down);
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
    match last_match {
        Some(m) => vstack((
            top,
            proposal_card(
                m,
                library,
                matches!(now_playing, NowPlaying::Idle),
                now,
                dispatch,
            ),
        ))
        .spacing(8.0)
        .into(),
        None => top,
    }
}

/// The Now playing placeholder copy: the plain idle pair, or the
/// detection-down pair when the watcher never started.
pub(crate) fn idle_placeholder_copy(detection_down: bool) -> (&'static str, &'static str) {
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
    let title = text_block(match_title(m, library))
        .font_size(20.0)
        .semibold()
        .wrap();
    let mut children: Vec<Element> =
        vec![title.into(), caption(match_caption(m, idle, now)).into()];
    let rows = fact_rows(m);
    if !rows.is_empty() {
        children.push(facts_grid(&rows));
    }
    if m.confidence == Confidence::Unmatched {
        children.push(
            button("Add to library")
                .on_click(move || dispatch.call(Command::AddProposedToLibrary))
                .horizontal_alignment(HorizontalAlignment::Left)
                .into(),
        );
    }
    card_frame(vstack(children).spacing(4.0)).into()
}

/// The library entry's title when the proposal resolves to one, else the
/// parsed title, else the raw player title.
pub(crate) fn match_title(m: &ProposedMatch, library: &[LibraryEntry]) -> String {
    let entry = m
        .entry
        .and_then(|id| library.iter().find(|entry| entry.id == id));
    if let Some(entry) = entry {
        entry.title.clone()
    } else if !m.parsed_title.is_empty() {
        m.parsed_title.clone()
    } else {
        m.raw_title.clone()
    }
}

pub(crate) fn match_caption(m: &ProposedMatch, idle: bool, now: SystemTime) -> String {
    let mut parts = Vec::new();
    if let Some(episode) = m.episode {
        parts.push(format!("Episode {episode}"));
    }
    parts.push(m.confidence.label().to_owned());
    if idle {
        parts.push(format!("Last seen in {}", m.player));
        parts.push(age_of(m.at, now));
    }
    parts.join(" \u{b7} ")
}

/// Every parsed element, or the persisted scalars after a reload has
/// emptied `elements`.
pub(crate) fn fact_rows(m: &ProposedMatch) -> Vec<(String, String)> {
    if !m.elements.is_empty() {
        return m
            .elements
            .iter()
            .map(|(label, value)| (fact_label(label), value.clone()))
            .collect();
    }
    let mut rows = Vec::new();
    if !m.parsed_title.is_empty() {
        rows.push(("Title".to_owned(), m.parsed_title.clone()));
    }
    if let Some(episode) = m.episode {
        rows.push(("Episode".to_owned(), episode.to_string()));
    }
    if let Some(season) = m.season {
        rows.push(("Season".to_owned(), season.to_string()));
    }
    if let Some(group) = &m.release_group {
        rows.push(("Group".to_owned(), group.clone()));
    }
    rows
}

fn fact_label(label: &str) -> String {
    let mut label = label.replace('_', " ");
    if let Some(first) = label.get_mut(..1) {
        first.make_ascii_uppercase();
    }
    label
}

fn facts_grid(rows: &[(String, String)]) -> Element {
    let cells: Vec<Element> = rows
        .iter()
        .enumerate()
        .flat_map(|(index, (label, value))| {
            let row = index as i32;
            [
                caption(label.clone()).grid_row(row).grid_column(0).into(),
                text_block(value.clone())
                    .wrap()
                    .grid_row(row)
                    .grid_column(1)
                    .into(),
            ]
        })
        .collect();
    grid(cells)
        .rows(std::iter::repeat_n(GridLength::Auto, rows.len()))
        .columns([GridLength::Auto, GridLength::STAR])
        .row_spacing(6.0)
        .column_spacing(16.0)
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

fn settings(
    settings: &Settings,
    dir: &DataDir,
    dispatch: Dispatch<Command>,
    folder_action: Option<String>,
    set_folder_action: SetState<Option<String>>,
) -> Element {
    let root = dir.root().to_path_buf();
    let open_folder = move || set_folder_action.call(Some(debug::open_folder(&root)));
    let open_diagnostics = {
        let dispatch = dispatch.clone();
        move || dispatch.call(Command::SelectPage(Page::Debug))
    };
    let app = debug::AppInfo::current();
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
            caption(format!("Ryuuji {} · {} build", app.version, app.profile)),
        ))
        .spacing(24.0)
        .max_width(CONTENT_MAX_WIDTH),
    )
    .into()
}

/// The theme picker, a drop-down like Windows Settings' "Choose your mode".
fn theme_card(theme: ThemePreference, dispatch: Dispatch<Command>) -> Border {
    let combo = ComboBox::new(ThemePreference::ALL.map(ThemePreference::label))
        .selected_index(theme_index(theme))
        .on_selection_changed(move |index: i32| {
            let chosen = usize::try_from(index)
                .ok()
                .and_then(|index| ThemePreference::ALL.get(index));
            if let Some(chosen) = chosen {
                dispatch.call(Command::SetTheme(*chosen));
            }
        })
        .min_width(160.0);
    card(
        None,
        "Theme",
        "Follow Windows, or pick light or dark.",
        combo.into(),
    )
}

fn theme_index(theme: ThemePreference) -> i32 {
    ThemePreference::ALL
        .iter()
        .position(|candidate| *candidate == theme)
        .and_then(|index| i32::try_from(index).ok())
        .unwrap_or(-1)
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
    use ryuuji_core::{Confidence, MatchOutcome, NewEntry, Ryuuji, WatchStatus};

    use super::*;

    #[test]
    fn theme_index_round_trips_over_every_preference() {
        for theme in ThemePreference::ALL {
            let index = usize::try_from(theme_index(theme)).unwrap();
            assert_eq!(ThemePreference::ALL[index], theme);
        }
    }

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
            entry: None,
            confidence: Confidence::Exact,
            outcome: MatchOutcome::Proposed,
            player: "mpv".to_owned(),
            at: std::time::SystemTime::UNIX_EPOCH,
            elements: Vec::new(),
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
            "Episode 1 \u{b7} Exact match \u{b7} Last seen in mpv \u{b7} 0 s ago"
        );
        assert_eq!(
            match_caption(&proposal(), false, SystemTime::UNIX_EPOCH),
            "Exact match"
        );
    }

    #[test]
    fn match_caption_appends_last_seen_and_age_when_idle() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(2 * 3600);
        assert_eq!(
            match_caption(&proposal(), true, now),
            "Exact match \u{b7} Last seen in mpv \u{b7} 2 h ago"
        );
        assert_eq!(match_caption(&proposal(), false, now), "Exact match");
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
    fn match_title_prefers_the_entry_then_parsed_then_raw() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = DataDir::at(tmp.path()).unwrap();
        let mut app = Ryuuji::open(&dir).unwrap();
        app.dispatch(Command::AddEntry(NewEntry {
            title: "Frieren: Beyond Journey's End".to_owned(),
            status: WatchStatus::Watching,
            progress: 0,
            total: None,
        }));
        let library = app.state().library.clone();
        let entry = ProposedMatch {
            entry: Some(library[0].id),
            ..proposal()
        };
        assert_eq!(
            match_title(&entry, &library),
            "Frieren: Beyond Journey's End"
        );
        assert_eq!(match_title(&proposal(), &library), "Parsed");
        let raw = ProposedMatch {
            parsed_title: String::new(),
            ..proposal()
        };
        assert_eq!(match_title(&raw, &library), "raw.mkv");
    }

    #[test]
    fn fact_rows_fall_back_to_scalars_when_elements_are_empty() {
        let m = ProposedMatch {
            episode: Some(3),
            season: Some(2),
            release_group: Some("Subs".to_owned()),
            ..proposal()
        };
        assert_eq!(
            fact_rows(&m),
            vec![
                ("Title".to_owned(), "Parsed".to_owned()),
                ("Episode".to_owned(), "3".to_owned()),
                ("Season".to_owned(), "2".to_owned()),
                ("Group".to_owned(), "Subs".to_owned()),
            ]
        );
        let m = ProposedMatch {
            elements: vec![("anime_title".to_owned(), "Show".to_owned())],
            ..m
        };
        assert_eq!(
            fact_rows(&m),
            vec![("Anime title".to_owned(), "Show".to_owned())]
        );
    }

    #[test]
    fn fact_label_humanises_snake_case() {
        assert_eq!(fact_label("release_group"), "Release group");
        assert_eq!(fact_label("anime_title"), "Anime title");
        assert_eq!(fact_label("source"), "Source");
    }
}
