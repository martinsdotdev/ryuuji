//! Page bodies. The Library lists real entries, Settings holds the theme
//! switcher and the way into Diagnostics, and Now playing is a placeholder in
//! the symbolic empty-state style the UI-direction research settled on
//! (heading required, neutral tone). The Diagnostics body itself is built by
//! the caller, so this module never sees the core handle or the log buffer.

use ryuuji_core::{
    AppState, Command, DataDir, LibraryEntry, Notice, NowPlaying, Page, Settings, StoreError,
    ThemePreference, error_chain,
};
use windows_reactor::*;

const PAGE_PADDING: f64 = 24.0;

/// Renders the notice bar and the body for the currently selected page.
pub fn render(
    state: &AppState,
    dispatch: Dispatch<Command>,
    debug: impl FnOnce() -> Element,
) -> Element {
    let page: Element = match state.page {
        Page::Library => library(&state.library),
        Page::NowPlaying => now_playing(&state.now_playing),
        Page::Settings => settings(&state.settings, dispatch.clone()),
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
        return placeholder("Your library is empty", "Shows you track will appear here.");
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

fn now_playing(now_playing: &NowPlaying) -> Element {
    match now_playing {
        NowPlaying::Idle => placeholder(
            "Nothing playing",
            "Open an episode in your player and it will show up here.",
        ),
        NowPlaying::Detecting => placeholder(
            "Detecting…",
            "A player is open; waiting for something recognisable.",
        ),
        NowPlaying::Playing {
            title,
            episode,
            player,
        } => placeholder(
            format!("{title} · Episode {episode}"),
            format!("Playing in {player}."),
        ),
    }
}

fn settings(settings: &Settings, dispatch: Dispatch<Command>) -> Element {
    let selected = ThemePreference::ALL
        .iter()
        .position(|theme| *theme == settings.theme)
        .map_or(-1, |index| index as i32);
    let theme = RadioButtons::new(ThemePreference::ALL.map(ThemePreference::label))
        .header("Theme")
        .selected_index(selected)
        .on_selection_changed({
            let dispatch = dispatch.clone();
            move |index: i32| {
                let theme = usize::try_from(index)
                    .ok()
                    .and_then(|index| ThemePreference::ALL.get(index));
                if let Some(theme) = theme {
                    dispatch.call(Command::SetTheme(*theme));
                }
            }
        });
    let diagnostics = button(Page::Debug.label())
        .on_click(move || dispatch.call(Command::SelectPage(Page::Debug)));
    vstack((theme, diagnostics)).spacing(16.0).into()
}

/// Symbolic placeholder: a heading and one line of body text on a card.
fn placeholder(heading: impl Into<String>, body: impl Into<String>) -> Element {
    border(
        vstack((
            text_block(heading).font_size(20.0).semibold(),
            text_block(body).foreground(ThemeRef::SecondaryText).wrap(),
        ))
        .spacing(4.0),
    )
    .background(ThemeRef::CardBackground)
    .border_brush(ThemeRef::CardStroke)
    .border_thickness(Thickness::uniform(1.0))
    .corner_radius(4.0)
    .padding(Thickness::uniform(16.0))
    .into()
}
