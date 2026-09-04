//! The Fluent building blocks the Settings and Diagnostics pages share:
//! section headers, cards and captions in the Windows Settings idiom, the
//! tables and enum pickers both pages build, and the relative-age text.

use std::path::Path;
use std::process::Command as Process;
use std::time::{Duration, SystemTime};

use tracing::warn;
use windows_reactor::*;

pub(crate) const CONTENT_MAX_WIDTH: f64 = 1000.0;
pub(crate) const ICON_FONT: &str = "Segoe Fluent Icons";
pub(crate) const FOLDER_GLYPH: &str = "\u{E8B7}";
pub(crate) const REPAIR_GLYPH: &str = "\u{E90F}";
pub(crate) const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
pub(crate) const BUILD_PROFILE: &str = if cfg!(debug_assertions) {
    "debug"
} else {
    "release"
};

pub(crate) fn section(title: &str) -> TextBlock {
    text_block(title).semibold().margin(Thickness {
        top: 8.0,
        ..Thickness::default()
    })
}

/// Secondary 12pt text: descriptions, table headers, outcomes.
pub(crate) fn caption(text: impl Into<String>) -> TextBlock {
    text_block(text)
        .foreground(ThemeRef::SecondaryText)
        .font_size(12.0)
}

/// A settings card: icon, title and description on the left, the control on
/// the right.
pub(crate) fn card(
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

pub(crate) fn heading(title: &str, description: impl Into<String>) -> StackPanel {
    vstack((text_block(title).wrap(), caption(description).wrap())).spacing(2.0)
}

pub(crate) fn card_frame(child: impl Into<Element>) -> Border {
    border(child)
        .background(ThemeRef::CardBackground)
        .border_brush(ThemeRef::CardStroke)
        .border_thickness(Thickness::uniform(1.0))
        .corner_radius(4.0)
        .padding(Thickness::uniform(16.0))
}

/// A grid of uniform rows: optional header, then one row per cell array.
/// Callers hand over unplaced cells and the builder assigns every
/// `grid_row`/`grid_column`, so a row count can never drift from the rows.
pub(crate) struct Table<const N: usize> {
    columns: [GridLength; N],
    row_spacing: f64,
    column_spacing: f64,
    header: Option<[&'static str; N]>,
}

pub(crate) fn table<const N: usize>(columns: [GridLength; N]) -> Table<N> {
    Table {
        columns,
        row_spacing: 0.0,
        column_spacing: 0.0,
        header: None,
    }
}

impl<const N: usize> Table<N> {
    pub(crate) fn spacing(mut self, row: f64, column: f64) -> Table<N> {
        self.row_spacing = row;
        self.column_spacing = column;
        self
    }

    pub(crate) fn header(mut self, titles: [&'static str; N]) -> Table<N> {
        self.header = Some(titles);
        self
    }

    /// Places the cells and closes the builder. `rows` is consumed here
    /// rather than stored so the row count comes from the cells themselves.
    pub(crate) fn rows(self, rows: impl IntoIterator<Item = [TextBlock; N]>) -> Grid {
        let offset = i32::from(self.header.is_some());
        let mut cells: Vec<Element> = Vec::new();
        if let Some(titles) = self.header {
            cells.extend(place(titles.map(caption), 0));
        }
        let mut count = offset;
        for row in rows {
            cells.extend(place(row, count));
            count += 1;
        }
        grid(cells)
            .rows(std::iter::repeat_n(GridLength::Auto, count as usize))
            .columns(self.columns)
            .row_spacing(self.row_spacing)
            .column_spacing(self.column_spacing)
    }
}

fn place<const N: usize>(cells: [TextBlock; N], row: i32) -> [Element; N] {
    let mut column = 0;
    cells.map(|cell| {
        let placed = cell.grid_row(row).grid_column(column).into();
        column += 1;
        placed
    })
}

/// A drop-down over a fixed set of variants, reporting the chosen one.
pub(crate) fn enum_picker<T: Copy + PartialEq + 'static>(
    options: &'static [T],
    label: fn(T) -> &'static str,
    selected: T,
    on_pick: impl Fn(T) + 'static,
) -> ComboBox {
    let index = options
        .iter()
        .position(|candidate| *candidate == selected)
        .and_then(|index| i32::try_from(index).ok())
        .unwrap_or(-1);
    ComboBox::new(options.iter().map(|option| label(*option)))
        .selected_index(index)
        .on_selection_changed(move |index: i32| {
            let chosen = usize::try_from(index)
                .ok()
                .and_then(|index| options.get(index));
            if let Some(chosen) = chosen {
                on_pick(*chosen);
            }
        })
}

/// Opens `root` in Explorer and describes the outcome for the page.
pub(crate) fn open_folder(root: &Path) -> String {
    match Process::new("explorer").arg(root).spawn() {
        Ok(_) => "Opened data folder".to_owned(),
        Err(err) => {
            warn!(%err, path = %root.display(), "could not open the data folder");
            format!("Could not open folder: {err}")
        }
    }
}

/// How often a page showing live values redraws itself.
const REFRESH: Duration = Duration::from_secs(2);

/// Rerenders the calling component every [`REFRESH`] while `running`. The
/// bumped counter has no reader; forcing the rerender is the whole point, so
/// the page gathers fresh values and its relative ages stay current. The dep
/// is a plain bool, so the timer is created on the flip to true and dropped
/// by the cleanup on the flip to false or when the page unmounts.
pub(crate) fn use_refresh(cx: &mut RenderCx, running: bool) {
    let (_tick, bump) = cx.use_reducer(0u32);
    cx.use_effect_with_cleanup(running, move || {
        if !running {
            return None;
        }
        match DispatcherTimer::new(REFRESH, move || {
            bump.call(|n| n.wrapping_add(1));
        }) {
            Ok(timer) => Some(move || drop(timer)),
            Err(err) => {
                warn!(%err, "refresh timer not started");
                None
            }
        }
    });
}

/// "2 h ago"-style age of `modified` at `now`.
pub(crate) fn age_of(modified: SystemTime, now: SystemTime) -> String {
    now.duration_since(modified)
        .map_or_else(|_| "in the future".to_owned(), age_text)
}

pub(crate) fn age_text(age: Duration) -> String {
    let secs = age.as_secs();
    let amount = match secs {
        0..60 => format!("{secs} s"),
        60..3_600 => format!("{} min", secs / 60),
        3_600..86_400 => format!("{} h", secs / 3_600),
        _ => format!("{} d", secs / 86_400),
    };
    format!("{amount} ago")
}

#[cfg(test)]
mod tests {
    use ryuuji_core::ThemePreference;

    use super::*;
    use crate::debug::EventLevelFilter;

    /// The picker maps a variant to its index and back; a variant missing
    /// from `options` would select nothing rather than the wrong row.
    #[test]
    fn enum_picker_selects_the_index_of_every_variant() {
        fn index_of<T: Copy + PartialEq + 'static>(
            options: &'static [T],
            label: fn(T) -> &'static str,
            selected: T,
        ) -> i32 {
            enum_picker(options, label, selected, |_| {}).selected_index
        }

        for (position, theme) in ThemePreference::ALL.iter().enumerate() {
            let index = index_of(&ThemePreference::ALL, ThemePreference::label, *theme);
            assert_eq!(index, position as i32);
        }
        for (position, filter) in EventLevelFilter::ALL.iter().enumerate() {
            let index = index_of(&EventLevelFilter::ALL, EventLevelFilter::label, *filter);
            assert_eq!(index, position as i32);
        }
    }

    #[test]
    fn ages_round_to_the_largest_whole_unit() {
        assert_eq!(age_text(Duration::from_secs(12)), "12 s ago");
        assert_eq!(age_text(Duration::from_secs(150)), "2 min ago");
        assert_eq!(age_text(Duration::from_secs(7_200)), "2 h ago");
        assert_eq!(age_text(Duration::from_secs(200_000)), "2 d ago");
    }
}
