//! The Fluent building blocks the Settings and Diagnostics pages share:
//! section headers, cards and captions in the Windows Settings idiom.

use windows_reactor::*;

pub(crate) const CONTENT_MAX_WIDTH: f64 = 1000.0;
pub(crate) const ICON_FONT: &str = "Segoe Fluent Icons";
pub(crate) const FOLDER_GLYPH: &str = "\u{E8B7}";
pub(crate) const REPAIR_GLYPH: &str = "\u{E90F}";

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
