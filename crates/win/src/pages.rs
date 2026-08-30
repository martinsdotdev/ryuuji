//! Page bodies. The Library lists real entries, Settings is laid out in the
//! Windows Settings idiom (section headers and cards) with the theme picker,
//! the data folder and the way into Diagnostics, and Now playing is a
//! placeholder in the symbolic empty-state style the UI-direction research
//! settled on (heading required, neutral tone). The Diagnostics body itself is
//! built by the caller, so this module never sees the core handle or the log
//! buffer.

use ryuuji_core::{
    AppState, Command, DataDir, LibraryEntry, Notice, NowPlaying, Page, Settings, StoreError,
    ThemePreference, error_chain,
};
use windows_reactor::*;

use crate::debug;

const PAGE_PADDING: f64 = 24.0;
const CONTENT_MAX_WIDTH: f64 = 1000.0;
const ICON_FONT: &str = "Segoe Fluent Icons";
const FOLDER_GLYPH: &str = "\u{E8B7}";
const REPAIR_GLYPH: &str = "\u{E90F}";

/// Renders the notice bar and the body for the currently selected page.
pub fn render(
    state: &AppState,
    dispatch: Dispatch<Command>,
    dir: &DataDir,
    debug: impl FnOnce() -> Element,
) -> Element {
    let page: Element = match state.page {
        Page::Library => library(&state.library),
        Page::NowPlaying => now_playing(&state.now_playing),
        Page::Settings => settings(&state.settings, dir, dispatch.clone()),
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

fn settings(settings: &Settings, dir: &DataDir, dispatch: Dispatch<Command>) -> Element {
    let root = dir.root().to_path_buf();
    let open_folder = move || {
        debug::open_folder(&root);
    };
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
                    button("Open folder").on_click(open_folder).into(),
                ),
                card(
                    Some(REPAIR_GLYPH),
                    "Diagnostics",
                    "Paths, schema version and recent log events",
                    button("Open diagnostics").on_click(open_diagnostics).into(),
                ),
            ))
            .spacing(8.0),
            text_block(format!("Ryuuji {} · {} build", app.version, app.profile))
                .foreground(ThemeRef::SecondaryText)
                .font_size(12.0),
        ))
        .spacing(24.0)
        .max_width(CONTENT_MAX_WIDTH),
    )
    .into()
}

fn section(title: &str) -> TextBlock {
    text_block(title).semibold().margin(Thickness {
        top: 8.0,
        ..Thickness::default()
    })
}

/// A settings card: icon, title and description on the left, the control on
/// the right.
fn card(
    icon: Option<&str>,
    title: &str,
    description: impl Into<String>,
    control: Element,
) -> Border {
    let icon: Element = match icon {
        Some(glyph) => text_block(glyph)
            .font_family(ICON_FONT)
            .font_size(20.0)
            .margin(Thickness {
                right: 16.0,
                ..Thickness::default()
            })
            .vertical_alignment(VerticalAlignment::Center)
            .grid_column(0)
            .into(),
        None => Element::Empty,
    };
    card_frame(
        grid(vec![
            icon,
            heading(title, description).grid_column(1).into(),
            border(control)
                .vertical_alignment(VerticalAlignment::Center)
                .grid_column(2)
                .into(),
        ])
        .columns([GridLength::Auto, GridLength::STAR, GridLength::Auto]),
    )
}

fn heading(title: &str, description: impl Into<String>) -> StackPanel {
    vstack((
        text_block(title),
        text_block(description)
            .foreground(ThemeRef::SecondaryText)
            .font_size(12.0)
            .wrap(),
    ))
    .spacing(2.0)
}

fn card_frame(child: impl Into<Element>) -> Border {
    border(child)
        .background(ThemeRef::CardBackground)
        .border_brush(ThemeRef::CardStroke)
        .border_thickness(Thickness::uniform(1.0))
        .corner_radius(4.0)
        .padding(Thickness::uniform(16.0))
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
    use super::*;

    #[test]
    fn theme_index_round_trips_over_every_preference() {
        for theme in ThemePreference::ALL {
            let index = usize::try_from(theme_index(theme)).unwrap();
            assert_eq!(ThemePreference::ALL[index], theme);
        }
    }
}
