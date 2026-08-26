//! Page bodies for the first window. Every page is a placeholder for now:
//! a heading plus one line of guidance, in the symbolic empty-state style
//! the UI-direction research settled on (heading required, neutral tone).

use ryuuji_core::{AppState, NowPlaying, Page};
use windows_reactor::*;

const PAGE_PADDING: f64 = 24.0;

/// Renders the body for the currently selected page.
pub fn render(state: &AppState) -> Element {
    let page: Element = match state.page {
        Page::Library => library(state),
        Page::NowPlaying => now_playing(&state.now_playing),
        Page::Settings => settings(),
    };

    border(page)
        .padding(Thickness::uniform(PAGE_PADDING))
        .into()
}

fn library(state: &AppState) -> Element {
    if state.library.is_empty() {
        placeholder("Your library is empty", "Shows you track will appear here.")
    } else {
        // Real rows arrive with the library slice; until then, a count.
        placeholder("Library", format!("{} shows tracked.", state.library.len()))
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
