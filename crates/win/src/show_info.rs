//! A show's info as the pages draw it: the cover, the synopsis and the
//! details. They come from the first service the show is paired with and
//! name none, since nothing about them needs the person.

use std::cell::RefCell;
use std::fmt;
use std::path::Path;
use std::rc::Rc;

use ryuuji_core::{
    AppState, Described, DescribedAt, EntryId, Format, LibraryEntry, ListStatus, Ryuuji, Service,
    ShowInfo, normalize_title,
};
use tracing::warn;
use windows_reactor::*;

use crate::ui::{ICON_FONT, card_frame, dot_joined, episodes_text, facts_table};

const COVER_WIDTH: f64 = 160.0;
const COVER_HEIGHT: f64 = 240.0;
const PICTURE_GLYPH: &str = "\u{E8B9}";

/// Where a page reads a show's info: the core, compared by identity, and
/// which description to read. That changes each time the show is described
/// again, so a page reads the store only then.
#[derive(Clone)]
pub(crate) struct Source {
    core: Rc<RefCell<Ryuuji>>,
    at: Option<DescribedAt>,
}

impl PartialEq for Source {
    fn eq(&self, other: &Source) -> bool {
        Rc::ptr_eq(&self.core, &other.core) && self.at == other.at
    }
}

impl fmt::Debug for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Source")
            .field("at", &self.at)
            .finish_non_exhaustive()
    }
}

/// Where to read what was kept about `entry` from the first service, in
/// service order, it is linked to and was described by.
pub(crate) fn source(
    state: &AppState,
    core: &Rc<RefCell<Ryuuji>>,
    entry: Option<EntryId>,
) -> Source {
    let at = entry.and_then(|entry| {
        Service::ALL.into_iter().find_map(|service| {
            let media = state.sync.listing(entry, service)?.tie.linked()?;
            state
                .sync
                .described
                .iter()
                .find(|known| known.service == service && known.media == media.id)
                .copied()
        })
    });
    Source {
        core: Rc::clone(core),
        at,
    }
}

/// The info `source` names, read from the store when it names another.
pub(crate) fn use_described(cx: &mut RenderCx, source: &Source) -> Option<Described> {
    let core = Rc::clone(&source.core);
    let at = source.at;
    cx.use_memo(at, move || {
        let at = at?;
        core.borrow()
            .show_info(at.service, at.media)
            .unwrap_or_else(|err| {
                warn!(error = %err, "show info unreadable");
                None
            })
    })
}

/// `Watching · 7 of 28 episodes`: the show's status and how far the person
/// is, or only the status when nothing is watched and the length is unknown.
pub(crate) fn status_line(entry: &LibraryEntry) -> String {
    let status = ListStatus::of(entry).label();
    match (entry.progress, entry.total) {
        (progress, Some(total)) => format!("{status} \u{b7} {progress} of {total} episodes"),
        (0, None) => status.to_owned(),
        (progress, None) => format!("{status} \u{b7} {}", episodes_text(progress)),
    }
}

/// `TV · 28 episodes`: the format and the length, each when it is known.
pub(crate) fn format_line(format: Option<Format>, episodes: Option<u32>) -> Option<String> {
    dot_joined([
        format.map(|format| format.label().to_owned()),
        episodes.map(episodes_text),
    ])
}

/// The cover with `content` beside it, the way Now playing's card and the
/// show's page open.
pub(crate) fn beside_cover(described: Option<&Described>, content: impl Into<Element>) -> Grid {
    grid((
        border(cover(described)).grid_column(0),
        border(content).grid_column(1),
    ))
    .columns([GridLength::Pixel(COVER_WIDTH), GridLength::STAR])
    .column_spacing(24.0)
}

/// The cover, or a picture glyph on a plain card while there is none.
fn cover(described: Option<&Described>) -> Element {
    match described.and_then(|described| described.cover.as_deref()) {
        Some(path) => Image::new_with_uri(file_uri(path))
            .stretch(Stretch::UniformToFill)
            .width(COVER_WIDTH)
            .height(COVER_HEIGHT)
            .into(),
        None => border(
            text_block(PICTURE_GLYPH)
                .font_family(ICON_FONT)
                .font_size(32.0)
                .foreground(ThemeRef::SecondaryText)
                .horizontal_alignment(HorizontalAlignment::Center)
                .vertical_alignment(VerticalAlignment::Center),
        )
        .width(COVER_WIDTH)
        .height(COVER_HEIGHT)
        .background(ThemeRef::SubtleFill)
        .border_brush(ThemeRef::CardStroke)
        .border_thickness(Thickness::uniform(1.0))
        .corner_radius(4.0)
        .into(),
    }
}

/// The synopsis card, when the service wrote one.
pub(crate) fn synopsis(info: &ShowInfo) -> Option<Element> {
    let text = info.description.as_ref().filter(|text| !text.is_empty())?;
    Some(
        card_frame(
            vstack((
                text_block("Synopsis").semibold(),
                text_block(text.clone()).wrap(),
            ))
            .spacing(8.0),
        )
        .into(),
    )
}

/// The details card, when there are any.
pub(crate) fn details(info: &ShowInfo, shown_title: &str) -> Option<Element> {
    let rows = detail_rows(info, shown_title);
    if rows.is_empty() {
        return None;
    }
    Some(
        card_frame(vstack((text_block("Details").semibold(), facts_table(rows))).spacing(12.0))
            .into(),
    )
}

/// Format, when it aired, genres, studio and the other names the show goes
/// by, each only when the service says it.
fn detail_rows(info: &ShowInfo, shown_title: &str) -> Vec<(&'static str, String)> {
    let mut rows = Vec::new();
    if let Some(format) = format_line(info.format, info.episodes) {
        rows.push(("Format", format));
    }
    if let Some(aired) = aired(info) {
        rows.push(("Aired", aired));
    }
    if !info.genres.is_empty() {
        rows.push(("Genres", info.genres.join(", ")));
    }
    if let Some(studio) = &info.studio {
        rows.push(("Studio", studio.clone()));
    }
    let shown = normalize_title(shown_title);
    let others: Vec<&str> = info
        .titles
        .iter()
        .filter(|title| normalize_title(title) != shown)
        .map(String::as_str)
        .collect();
    if !others.is_empty() {
        rows.push(("Also known as", others.join(", ")));
    }
    rows
}

/// `Fall 2023 · finished`: the season it began and where it stands.
fn aired(info: &ShowInfo) -> Option<String> {
    let season = match (info.season, info.season_year) {
        (Some(season), Some(year)) => Some(format!("{} {year}", season.label())),
        (None, Some(year)) => Some(year.to_string()),
        (Some(_) | None, None) => None,
    };
    let status = info.status.map(|status| status.label().to_owned());
    dot_joined([season, status])
}

/// A `file:` URI for a path on this computer, with the characters a URI
/// reads as its own escaped.
fn file_uri(path: &Path) -> String {
    let mut uri = String::from("file:///");
    for ch in path.to_string_lossy().chars() {
        match ch {
            '\\' => uri.push('/'),
            ' ' => uri.push_str("%20"),
            '#' => uri.push_str("%23"),
            '%' => uri.push_str("%25"),
            '?' => uri.push_str("%3F"),
            other => uri.push(other),
        }
    }
    uri
}

#[cfg(test)]
mod tests {
    use ryuuji_core::{Airing, Format, Season};

    use super::*;

    fn frieren() -> ShowInfo {
        ShowInfo {
            titles: vec![
                "Sousou no Frieren".to_owned(),
                "Frieren: Beyond Journey's End".to_owned(),
                "葬送のフリーレン".to_owned(),
                "Frieren".to_owned(),
            ],
            format: Some(Format::Tv),
            season: Some(Season::Fall),
            season_year: Some(2023),
            status: Some(Airing::Finished),
            episodes: Some(28),
            genres: vec!["Adventure".to_owned(), "Drama".to_owned()],
            studio: Some("Madhouse".to_owned()),
            description: Some("After the party.".to_owned()),
        }
    }

    #[test]
    fn the_details_say_what_the_service_knows_without_repeating_the_title() {
        assert_eq!(
            detail_rows(&frieren(), "Frieren: Beyond Journey's End"),
            vec![
                ("Format", "TV \u{b7} 28 episodes".to_owned()),
                ("Aired", "Fall 2023 \u{b7} finished".to_owned()),
                ("Genres", "Adventure, Drama".to_owned()),
                ("Studio", "Madhouse".to_owned()),
                (
                    "Also known as",
                    "Sousou no Frieren, 葬送のフリーレン, Frieren".to_owned()
                ),
            ]
        );
        assert_eq!(detail_rows(&ShowInfo::default(), "Show"), vec![]);
        let airing = ShowInfo {
            format: Some(Format::TvShort),
            episodes: Some(1),
            season: None,
            status: Some(Airing::Ongoing),
            ..frieren()
        };
        let rows = detail_rows(&airing, "Frieren");
        assert_eq!(rows[0], ("Format", "TV short \u{b7} 1 episode".to_owned()));
        assert_eq!(rows[1], ("Aired", "2023 \u{b7} airing".to_owned()));
    }

    #[test]
    fn a_cover_path_becomes_a_file_uri() {
        assert_eq!(
            file_uri(Path::new(
                r"C:\Users\someone\App Data\covers\anilist-1#2.jpg"
            )),
            "file:///C:/Users/someone/App%20Data/covers/anilist-1%232.jpg"
        );
    }

    #[test]
    fn no_description_means_no_synopsis_card() {
        assert!(synopsis(&ShowInfo::default()).is_none());
        assert!(synopsis(&frieren()).is_some());
    }
}
