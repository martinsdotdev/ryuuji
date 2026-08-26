//! Page bodies. The Library lists real entries; Now playing and Settings are
//! placeholders in the symbolic empty-state style the UI-direction research
//! settled on (heading required, neutral tone).

use ryuuji_core::{
    AppState, Command, DataDir, LibraryEntry, Notice, NowPlaying, Page, StoreError, error_chain,
};
use windows_reactor::*;

const PAGE_PADDING: f64 = 24.0;

/// Renders the notice bar and the body for the currently selected page.
pub fn render(state: &AppState, dispatch: Dispatch<Command>) -> Element {
    let page: Element = match state.page {
        Page::Library => library(&state.library),
        Page::NowPlaying => now_playing(&state.now_playing),
        Page::Settings => settings(),
    };

    grid((
        notice(state.notice.as_ref(), dispatch).grid_row(0),
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

fn notice(notice: Option<&Notice>, dispatch: Dispatch<Command>) -> InfoBar {
    let (title, message) = match notice {
        Some(Notice::SaveFailed { detail }) => ("Couldn't save your change", detail.as_str()),
        None => ("", ""),
    };
    InfoBar::new(title)
        .message(message)
        .severity(InfoBarSeverity::Error)
        .is_open(notice.is_some())
        .on_closed(move || dispatch.call(Command::DismissNotice))
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

fn settings() -> Element {
    placeholder("Settings", "Nothing to configure yet.")
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
