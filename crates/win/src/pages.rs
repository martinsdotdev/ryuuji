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

const TILE_WIDTH: f64 = 132.0;
const TILE_HEIGHT: f64 = 88.0;
const TILE_STROKE: f64 = 1.0;
const TILE_HALF_WIDTH: f64 = (TILE_WIDTH - 2.0 * TILE_STROKE) / 2.0;
const RAIL_WIDTH: f64 = 36.0;
const CONTENT_INSET: f64 = 12.0;
/// How much of each content bar lies left of the tile's vertical midline.
const BAR_LEFT_RUN: f64 = TILE_HALF_WIDTH - RAIL_WIDTH - CONTENT_INSET;
const BAR_WIDTHS: [f64; 2] = [64.0, 44.0];

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

/// The theme picker: one miniature window per preference, selected like a
/// GridView item.
fn theme_card(theme: ThemePreference, dispatch: Dispatch<Command>) -> Border {
    let tiles = grid_view(ThemePreference::ALL.to_vec(), |preference, _| {
        vstack((
            theme_tile(*preference),
            text_block(preference.label()).horizontal_alignment(HorizontalAlignment::Center),
        ))
        .spacing(6.0)
    })
    .with_key_selector(|preference| preference.tag().to_owned())
    .selection_mode(SelectionMode::Single)
    .selected_index(theme_index(theme))
    .on_selection_changed(move |index: i32| {
        let chosen = usize::try_from(index)
            .ok()
            .and_then(|index| ThemePreference::ALL.get(index));
        if let Some(chosen) = chosen {
            dispatch.call(Command::SetTheme(*chosen));
        }
    })
    .height(140.0);
    card_frame(
        vstack((
            heading("Theme", "Follow Windows, or pick light or dark."),
            tiles,
        ))
        .spacing(12.0),
    )
}

fn theme_index(theme: ThemePreference) -> i32 {
    ThemePreference::ALL
        .iter()
        .position(|candidate| *candidate == theme)
        .and_then(|index| i32::try_from(index).ok())
        .unwrap_or(-1)
}

/// Fixed colours for a theme tile. The tiles preview a theme, so they never
/// follow the live one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TilePalette {
    pub(crate) canvas: Color,
    pub(crate) surface: Color,
    pub(crate) accent: Color,
    pub(crate) bar: Color,
}

const LIGHT_PALETTE: TilePalette = TilePalette {
    canvas: Color::rgb(0xF3, 0xF3, 0xF3),
    surface: Color::rgb(0xFF, 0xFF, 0xFF),
    accent: Color::rgb(0x00, 0x5F, 0xB8),
    bar: Color::rgb(0xDA, 0xDA, 0xDA),
};

const DARK_PALETTE: TilePalette = TilePalette {
    canvas: Color::rgb(0x20, 0x20, 0x20),
    surface: Color::rgb(0x2B, 0x2B, 0x2B),
    accent: Color::rgb(0x4C, 0xC2, 0xFF),
    bar: Color::rgb(0x3A, 0x3A, 0x3A),
};

pub(crate) fn palette(theme: ThemePreference) -> TilePalette {
    match theme {
        ThemePreference::System | ThemePreference::Light => LIGHT_PALETTE,
        ThemePreference::Dark => DARK_PALETTE,
    }
}

/// The palettes of a tile's left and right halves. `System` shows one window
/// that is dark on the left and light on the right.
fn halves(theme: ThemePreference) -> (TilePalette, TilePalette) {
    match theme {
        ThemePreference::System => (DARK_PALETTE, LIGHT_PALETTE),
        ThemePreference::Light | ThemePreference::Dark => (palette(theme), palette(theme)),
    }
}

/// A miniature Ryuuji window: a nav rail with three pills and a content area
/// with two bars, split down the middle so each half can carry its own palette.
fn theme_tile(theme: ThemePreference) -> Border {
    let (left, right) = halves(theme);
    border(hstack((left_half(left), right_half(right))))
        .width(TILE_WIDTH)
        .height(TILE_HEIGHT)
        .corner_radius(4.0)
        .border_brush(ThemeRef::CardStroke)
        .border_thickness(Thickness::uniform(TILE_STROKE))
}

fn left_half(palette: TilePalette) -> Border {
    let rail = border(
        vstack((pill(palette.bar), pill(palette.accent), pill(palette.bar)))
            .spacing(6.0)
            .horizontal_alignment(HorizontalAlignment::Center)
            .margin(Thickness {
                top: 10.0,
                ..Thickness::default()
            }),
    )
    .background(palette.surface)
    .width(RAIL_WIDTH);
    let bars = content_bars(palette.bar, BAR_WIDTHS.map(|_| BAR_LEFT_RUN)).margin(Thickness {
        left: CONTENT_INSET,
        top: CONTENT_INSET,
        ..Thickness::default()
    });
    border(hstack((rail, bars)))
        .background(palette.canvas)
        .width(TILE_HALF_WIDTH)
}

fn right_half(palette: TilePalette) -> Border {
    let bars =
        content_bars(palette.bar, BAR_WIDTHS.map(|width| width - BAR_LEFT_RUN)).margin(Thickness {
            top: CONTENT_INSET,
            ..Thickness::default()
        });
    border(bars)
        .background(palette.canvas)
        .width(TILE_HALF_WIDTH)
}

fn content_bars(color: Color, widths: [f64; 2]) -> StackPanel {
    vstack(widths.map(|width| bar(color, width))).spacing(6.0)
}

fn pill(color: Color) -> Border {
    border(Element::Empty)
        .width(20.0)
        .height(4.0)
        .corner_radius(2.0)
        .background(color)
}

fn bar(color: Color, width: f64) -> Border {
    border(Element::Empty)
        .width(width)
        .height(6.0)
        .background(color)
        .horizontal_alignment(HorizontalAlignment::Left)
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
    fn light_and_dark_tiles_have_distinct_canvases() {
        assert_ne!(
            palette(ThemePreference::Light).canvas,
            palette(ThemePreference::Dark).canvas
        );
    }

    #[test]
    fn light_tile_surface_is_white() {
        assert_eq!(
            palette(ThemePreference::Light).surface,
            Color::rgb(0xFF, 0xFF, 0xFF)
        );
    }

    #[test]
    fn system_tile_is_dark_on_the_left_and_light_on_the_right() {
        assert_eq!(
            halves(ThemePreference::System),
            (
                palette(ThemePreference::Dark),
                palette(ThemePreference::Light)
            )
        );
    }

    #[test]
    fn theme_index_round_trips_over_every_preference() {
        for theme in ThemePreference::ALL {
            let index = usize::try_from(theme_index(theme)).unwrap();
            assert_eq!(ThemePreference::ALL[index], theme);
        }
    }

    #[test]
    fn content_bars_meet_at_the_tile_midline() {
        let right_runs = BAR_WIDTHS.map(|width| width - BAR_LEFT_RUN);
        assert!(right_runs.iter().all(|run| *run > 0.0), "{right_runs:?}");
        assert_eq!(RAIL_WIDTH + CONTENT_INSET + BAR_LEFT_RUN, TILE_HALF_WIDTH);
    }
}
