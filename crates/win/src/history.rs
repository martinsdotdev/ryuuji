//! The History page: every watch that reached its threshold and every value
//! the library took from a list, newest first, under the local day it shows
//! at, with Undo on a recording the show still stands at. Rows are read from
//! the core on each render rather than held in the app state, which the
//! shell clones on every dispatch; a page of them loads at a time and Show
//! older asks for the next.

use std::cell::RefCell;
use std::fmt;
use std::rc::Rc;
use std::time::SystemTime;

use ryuuji_core::{
    Command, EntryId, HistoryId, HistoryPage, LibraryEntry, Line, Link, ListStatus, ListUpdate,
    ListUpdateId, Recorded, Ryuuji, StatusMove, TimeZone, Watch, WatchOutcome, days, error_chain,
    time_of_day, week_start,
};
use windows_reactor::*;

use crate::ui::{
    self, CONTENT_MAX_WIDTH, ICON_FONT, MONO_FONT, caption, card_frame, episode_text, placeholder,
};

/// Rows loaded at first, and added by each Show older.
const PAGE_SIZE: usize = 200;

const EMPTY_HEADING: &str = "No episodes yet";
const EMPTY_BODY: &str = "An episode shows up here once you've watched half of it, or two \
                          minutes when the player doesn't report a length. Ryuuji lists it \
                          whether it recorded the episode or not, and says why when it didn't.";

/// The sources History reads. `library` names each row's show and decides
/// whether its Undo still applies.
#[derive(Clone)]
pub struct HistoryProps {
    pub dispatch: Dispatch<Command>,
    pub core: Rc<RefCell<Ryuuji>>,
    pub library: Vec<LibraryEntry>,
}

/// Identity for the handles, contents for the library. The reactor requires
/// `PartialEq` on props; `dispatch` is fresh every render, so this never
/// holds today.
impl PartialEq for HistoryProps {
    fn eq(&self, other: &HistoryProps) -> bool {
        self.dispatch == other.dispatch
            && Rc::ptr_eq(&self.core, &other.core)
            && self.library == other.library
    }
}

pub fn history(props: &HistoryProps, cx: &mut RenderCx) -> Element {
    let (limit, set_limit) = cx.use_state(PAGE_SIZE);
    let (_attempts, retry) = cx.use_reducer(0u32);
    let zone = ui::local_zone();
    let now = SystemTime::now();
    let read = {
        let core = props.core.borrow();
        core.history(None, limit).and_then(|page| {
            let recent = core.episodes_recorded_since(week_start(now, &zone))?;
            Ok((page, recent))
        })
    };
    let (page, recent) = match read {
        Ok(read) => read,
        Err(err) => {
            return unreadable(&error_chain(&err), move || {
                retry.call(|n: u32| n.wrapping_add(1));
            });
        }
    };
    if page.watches.is_empty() && page.updates.is_empty() {
        return placeholder(EMPTY_HEADING, EMPTY_BODY);
    }

    let dispatch = props.dispatch.clone();
    let list = list_view(items(&page, &props.library, now, &zone), move |item, _| {
        item_view(item, &dispatch, &set_limit, limit)
    })
    .with_key_selector(key)
    .selection_mode(SelectionMode::None);
    grid((
        text_block(summary_text(recent))
            .font_size(20.0)
            .semibold()
            .margin(Thickness {
                bottom: 8.0,
                ..Thickness::default()
            }),
        list.grid_row(1),
    ))
    .rows([GridLength::Auto, GridLength::STAR])
    .max_width(CONTENT_MAX_WIDTH)
    .into()
}

/// A read that failed changes nothing, so the page says so and offers to
/// read again.
fn unreadable(detail: &str, retry: impl Fn() + 'static) -> Element {
    card_frame(
        vstack((
            text_block("Couldn't read your history")
                .font_size(20.0)
                .semibold(),
            text_block(detail)
                .font_family(MONO_FONT)
                .wrap()
                .selectable(),
            text_block("Nothing was changed. Diagnostics in Settings shows the full error.")
                .foreground(ThemeRef::SecondaryText)
                .wrap(),
            button("Try again").on_click(retry),
        ))
        .spacing(8.0),
    )
    .into()
}

/// `5 episodes recorded in the last 7 days`, the count Taiga's idle Now
/// Playing page gives. Recordings only: what a list changed was not
/// watched here.
fn summary_text(episodes: u32) -> String {
    match episodes {
        0 => "No episodes recorded in the last 7 days".to_owned(),
        1 => "1 episode recorded in the last 7 days".to_owned(),
        n => format!("{n} episodes recorded in the last 7 days"),
    }
}

/// One entry of the list: a day's heading, a row, or the way to older rows
/// after a full page.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Item {
    /// Keyed by its first row, since a label can repeat across a page
    /// whose times are out of order.
    Day {
        label: String,
        first: RowId,
    },
    Row(Row),
    Older,
}

fn items(
    page: &HistoryPage,
    library: &[LibraryEntry],
    now: SystemTime,
    zone: &TimeZone,
) -> Vec<Item> {
    let mut items = Vec::new();
    for day in days(&page.watches, &page.updates, now, zone) {
        let rows: Vec<Row> = day
            .lines
            .iter()
            .map(|line| Row::of(*line, library, zone))
            .collect();
        items.push(Item::Day {
            label: day.label,
            first: rows[0].id,
        });
        items.extend(rows.into_iter().map(Item::Row));
    }
    if page.more {
        items.push(Item::Older);
    }
    items
}

fn key(item: &Item) -> String {
    match item {
        Item::Day { first, .. } => format!("day-{first}"),
        Item::Row(row) => row.id.to_string(),
        Item::Older => "older".to_owned(),
    }
}

/// The stored row a History row shows; written out, the list's key for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RowId {
    Watch(HistoryId),
    Update(ListUpdateId),
}

impl fmt::Display for RowId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RowId::Watch(id) => write!(f, "watch-{id}"),
            RowId::Update(id) => write!(f, "update-{id}"),
        }
    }
}

/// What a row shows, worked out from the stored row and the library so the
/// view only lays it out.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Row {
    id: RowId,
    mark: Mark,
    /// `Episode 8 · Frieren: Beyond Journey's End`; the show alone for an
    /// update, which is about the show and not an episode.
    title: String,
    /// The outcome, the marks on it and the player, joined by middle dots;
    /// for an update, what the list changed.
    detail: String,
    /// The file the player reported. An update came from no file.
    file: Option<String>,
    time: String,
    action: Action,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mark {
    Recorded,
    Declined,
    Added,
    Undone,
    Ignored,
    Updated,
}

impl Mark {
    /// Segoe Fluent Icons: CheckMark, Info, Add, Undo, Blocked, Sync.
    fn glyph(self) -> &'static str {
        match self {
            Mark::Recorded => "\u{E73E}",
            Mark::Declined => "\u{E946}",
            Mark::Added => "\u{E710}",
            Mark::Undone => "\u{E7A7}",
            Mark::Ignored => "\u{E733}",
            Mark::Updated => "\u{E895}",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    Undo(HistoryId),
    Undone,
    Nothing,
}

impl Row {
    fn of(line: Line<'_>, library: &[LibraryEntry], zone: &TimeZone) -> Row {
        match line {
            Line::Watch(watch) => Row::watch(watch, library, zone),
            Line::Update(update) => Row::update(update, library, zone),
        }
    }

    fn watch(watch: &Watch, library: &[LibraryEntry], zone: &TimeZone) -> Row {
        let (mark, outcome) = match watch.outcome {
            WatchOutcome::Recorded(recorded) => (
                if recorded.undone_at.is_some() {
                    Mark::Undone
                } else {
                    Mark::Recorded
                },
                recorded_text(&recorded),
            ),
            WatchOutcome::Declined(decline) => (Mark::Declined, decline.label().to_owned()),
            WatchOutcome::Added => (Mark::Added, "Added to library".to_owned()),
            WatchOutcome::Ignored => (Mark::Ignored, "Ignored".to_owned()),
        };
        // A moved row says where the episode went instead of what it wrote:
        // the write was taken back, so restating it would read as though the
        // episode still stood here. Only an undone recording can have gone
        // anywhere, which is the invariant the store writes in one step.
        let moved_to = watch
            .moved_to
            .filter(|_| mark == Mark::Undone)
            .map(|entry| format!("Undone, moved to {}", show_named(entry, library)));
        let mut detail = vec![moved_to.unwrap_or(outcome)];
        if watch.moved_from.is_some() {
            detail.push("moved here".to_owned());
        }
        if watch.added_at.is_some() && watch.outcome != WatchOutcome::Added {
            detail.push("added to library".to_owned());
        }
        if let Link::Likely(_) = watch.link {
            detail.push(watch.link.confidence().label().to_owned());
        }
        detail.push(watch.player.clone());
        let action = if watch.undoable(library) {
            Action::Undo(watch.id)
        } else if mark == Mark::Undone {
            Action::Undone
        } else {
            Action::Nothing
        };
        Row {
            id: RowId::Watch(watch.id),
            mark,
            title: row_title(watch, library),
            detail: detail.join(" \u{b7} "),
            file: Some(watch.raw_title.clone()),
            time: time_of_day(watch.shown_at(), zone),
            action,
        }
    }

    /// Nothing to undo: what a list changed is changed back on the list,
    /// and the next merge takes that up.
    fn update(update: &ListUpdate, library: &[LibraryEntry], zone: &TimeZone) -> Row {
        Row {
            id: RowId::Update(update.id),
            mark: Mark::Updated,
            title: show_named(update.entry, library),
            detail: update_text(update),
            file: None,
            time: time_of_day(update.at, zone),
            action: Action::Nothing,
        }
    }
}

/// `Recorded 1 → 2`, then where the episode moved the show when it moved
/// it. An undone recording says only what it wrote: the move went back with
/// the episode, and the row is dimmed for that.
fn recorded_text(recorded: &Recorded) -> String {
    let wrote = format!(
        "Recorded {} \u{2192} {}",
        recorded.progress_before, recorded.progress
    );
    match recorded.status.filter(|_| recorded.undone_at.is_none()) {
        Some(moved) => format!("{wrote} \u{b7} {}", status_move_text(moved)),
        None => wrote,
    }
}

/// Where a recording moved the show: `Completed` for the last episode,
/// `rewatch finished` when that episode ended a rewatch, `Watching` for a
/// show taken off Plan to watch, On hold or Dropped.
fn status_move_text(moved: StatusMove) -> &'static str {
    match (moved.before, moved.after) {
        (ListStatus::Rewatching, ListStatus::Completed) => "rewatch finished",
        (_, after) => after.label(),
    }
}

/// `Updated from AniList 3 → 5`, then the status the list moved the show to
/// when it moved it; `Updated from AniList · Completed` when only the status
/// moved.
fn update_text(update: &ListUpdate) -> String {
    let mut text = format!("Updated from {}", update.service.label());
    if update.progress != update.progress_before {
        text.push_str(&format!(
            " {} \u{2192} {}",
            update.progress_before, update.progress
        ));
    }
    if update.status != update.status_before {
        text.push_str(&format!(" \u{b7} {}", update.status.label()));
    }
    text
}

/// The show as the library names it, falling back to its id for an entry
/// the library no longer lists.
fn show_named(entry: EntryId, library: &[LibraryEntry]) -> String {
    library
        .iter()
        .find(|row| row.id == entry)
        .map_or_else(|| format!("entry {entry}"), |row| row.title.clone())
}

/// The episode, then the show as the library names it, falling back to the
/// title the parser read and then to the raw title.
fn row_title(watch: &Watch, library: &[LibraryEntry]) -> String {
    let show = watch
        .link
        .entry()
        .and_then(|id| library.iter().find(|entry| entry.id == id))
        .map(|entry| entry.title.as_str())
        .or_else(|| (!watch.parsed_title.is_empty()).then_some(watch.parsed_title.as_str()))
        .unwrap_or(&watch.raw_title);
    match &watch.episode {
        Some(episode) => format!("{} \u{b7} {show}", episode_text(episode)),
        None => show.to_owned(),
    }
}

fn item_view(
    item: &Item,
    dispatch: &Dispatch<Command>,
    set_limit: &SetState<usize>,
    limit: usize,
) -> Element {
    match item {
        Item::Day { label, .. } => text_block(label.clone())
            .semibold()
            .margin(Thickness {
                top: 12.0,
                bottom: 4.0,
                ..Thickness::default()
            })
            .into(),
        Item::Row(row) => row_view(row, dispatch.clone()),
        Item::Older => {
            let set_limit = set_limit.clone();
            button("Show older")
                .on_click(move || set_limit.call(limit + PAGE_SIZE))
                .into()
        }
    }
}

fn row_view(row: &Row, dispatch: Dispatch<Command>) -> Element {
    let glyph = text_block(row.mark.glyph())
        .font_family(ICON_FONT)
        .font_size(16.0)
        .foreground(if row.mark == Mark::Recorded {
            ThemeRef::AccentText
        } else {
            ThemeRef::SecondaryText
        })
        .vertical_alignment(VerticalAlignment::Center)
        .grid_column(0);
    let title = text_block(row.title.clone())
        .max_lines(1)
        .text_trimming(TextTrimming::CharacterEllipsis);
    let detail: Element = match &row.file {
        Some(file) => grid((
            caption(format!("{} \u{b7} ", row.detail)).grid_column(0),
            caption(file.clone())
                .font_family(MONO_FONT)
                .max_lines(1)
                .text_trimming(TextTrimming::CharacterEllipsis)
                .grid_column(1),
        ))
        .columns([GridLength::Auto, GridLength::STAR])
        .into(),
        None => caption(row.detail.clone()).into(),
    };
    let main = vstack(vec![title.into(), detail])
        .spacing(2.0)
        .vertical_alignment(VerticalAlignment::Center)
        .grid_column(1);
    let time = caption(row.time.clone())
        .vertical_alignment(VerticalAlignment::Center)
        .grid_column(2);
    let action: Element = match row.action {
        Action::Undo(id) => button("Undo")
            .on_click(move || dispatch.call(Command::UndoRecording(id)))
            .vertical_alignment(VerticalAlignment::Center)
            .grid_column(3)
            .into(),
        Action::Undone => caption("Undone")
            .vertical_alignment(VerticalAlignment::Center)
            .grid_column(3)
            .into(),
        Action::Nothing => Element::Empty,
    };
    let frame = border(
        grid(vec![glyph.into(), main.into(), time.into(), action])
            .columns([
                GridLength::Auto,
                GridLength::STAR,
                GridLength::Auto,
                GridLength::Auto,
            ])
            .column_spacing(16.0),
    )
    .background(ThemeRef::CardBackground)
    .border_brush(ThemeRef::CardStroke)
    .border_thickness(Thickness::uniform(1.0))
    .corner_radius(4.0)
    .padding(Thickness {
        left: 16.0,
        top: 8.0,
        right: 12.0,
        bottom: 8.0,
    })
    .min_height(56.0);
    // The pinned windows-reactor draws a rich text run's bold but not its
    // strikethrough, so the dimming and the Undone label mark an undone row.
    if row.mark == Mark::Undone {
        frame.opacity(0.55).into()
    } else {
        frame.into()
    }
}

#[cfg(test)]
mod tests {
    use std::time::UNIX_EPOCH;

    use ryuuji_core::{
        Account, Asked, Decline, EntryId, ListEntryId, ListState, Media, MediaId, NewEntry,
        NewRecording, Opened, ProposedMatch, Remote, Report, Service, Signed, Store, UserId,
        WatchStatus,
    };

    use super::*;

    fn show(title: &str) -> NewEntry {
        NewEntry {
            title: title.to_owned(),
            status: WatchStatus::Watching,
            progress: 0,
            total: None,
            rewatching: false,
        }
    }

    fn recording(entry: EntryId, episode: u32, raw_title: &str) -> NewRecording {
        NewRecording {
            watch: None,
            entry,
            episode: episode..=episode,
            raw_title: raw_title.to_owned(),
            parsed_title: String::new(),
            player: "mpv".to_owned(),
        }
    }

    fn proposal(raw_title: &str, parsed_title: &str, episode: u32, link: Link) -> ProposedMatch {
        ProposedMatch {
            raw_title: raw_title.to_owned(),
            parsed_title: parsed_title.to_owned(),
            episode: Some(episode..=episode),
            season: None,
            release_group: None,
            link,
            player: "mpv".to_owned(),
            at: SystemTime::UNIX_EPOCH,
        }
    }

    fn open_store() -> (tempfile::TempDir, Store) {
        let tmp = tempfile::tempdir().unwrap();
        let dir = ryuuji_core::DataDir::at(tmp.path()).unwrap();
        let Opened { store, .. } = Store::open(&dir).unwrap();
        (tmp, store)
    }

    fn watch_row(watch: &Watch, library: &[LibraryEntry]) -> Row {
        Row::of(Line::Watch(watch), library, &TimeZone::UTC)
    }

    #[test]
    fn the_summary_counts_episodes_in_words() {
        assert_eq!(summary_text(0), "No episodes recorded in the last 7 days");
        assert_eq!(summary_text(1), "1 episode recorded in the last 7 days");
        assert_eq!(summary_text(5), "5 episodes recorded in the last 7 days");
    }

    #[test]
    fn a_row_says_what_came_of_each_watch_and_offers_undo_only_where_it_applies() {
        let (_tmp, mut store) = open_store();
        let frieren = store.add(show("Frieren")).unwrap().id;
        let moved_on = store
            .record(recording(frieren, 1, "Frieren - 01.mkv"))
            .unwrap()
            .watch
            .id;
        let spice = store.add(show("Spice and Wolf")).unwrap().id;
        let undone = store
            .record(recording(spice, 1, "Spice - 01.mkv"))
            .unwrap()
            .watch
            .id;
        store.undo(undone).unwrap();
        let likely = store
            .decline(
                None,
                &proposal("Kusuriya - 05.mkv", "Kusuriya", 5, Link::Likely(frieren)),
                Decline::NotExact,
            )
            .unwrap()
            .id;
        let unmatched = store
            .decline(
                None,
                &proposal("Dandadan - 03.mkv", "Dandadan", 3, Link::Unmatched),
                Decline::NotExact,
            )
            .unwrap()
            .id;
        let mushishi = proposal("Mushishi - 01.mkv", "Mushishi", 1, Link::Unmatched);
        let added = store
            .add_to_library(show("Mushishi"), None, &mushishi)
            .unwrap();
        let other = proposal("Other - 04.mkv", "Other", 4, Link::Unmatched);
        let refused = store.add_to_library(show("Other"), None, &other).unwrap();
        store
            .decline(
                Some(refused.watch.id),
                &ProposedMatch {
                    link: Link::Exact(refused.entry.id),
                    ..other
                },
                Decline::NotNext,
            )
            .unwrap();
        let latest = store
            .record(recording(frieren, 2, "Frieren - 02.mkv"))
            .unwrap()
            .watch
            .id;

        // A correction: the episode leaves one show and lands on another,
        // both rows kept. Two shows of its own, so nothing above shifts.
        let bocchi = store.add(show("Bocchi")).unwrap().id;
        let kagurabachi = store.add(show("Kagurabachi")).unwrap().id;
        let strayed = store
            .record(recording(bocchi, 1, "Bocchi - 01.mkv"))
            .unwrap()
            .watch
            .id;
        let corrected = store.move_recording(strayed, kagurabachi).unwrap().watch.id;

        let library = store.entries().unwrap();
        let watches = store.history(None, 20).unwrap().watches;
        let rows: Vec<Row> = watches
            .iter()
            .map(|watch| watch_row(watch, &library))
            .collect();
        let row = |id: HistoryId| rows.iter().find(|row| row.id == RowId::Watch(id)).unwrap();
        let said = |id| {
            let row = row(id);
            (
                row.mark,
                row.title.as_str(),
                row.detail.as_str(),
                row.action,
            )
        };

        assert_eq!(
            said(latest),
            (
                Mark::Recorded,
                "Episode 2 \u{b7} Frieren",
                "Recorded 1 \u{2192} 2 \u{b7} mpv",
                Action::Undo(latest)
            )
        );
        assert_eq!(
            said(moved_on),
            (
                Mark::Recorded,
                "Episode 1 \u{b7} Frieren",
                "Recorded 0 \u{2192} 1 \u{b7} mpv",
                Action::Nothing
            )
        );
        assert_eq!(
            said(undone),
            (
                Mark::Undone,
                "Episode 1 \u{b7} Spice and Wolf",
                "Recorded 0 \u{2192} 1 \u{b7} mpv",
                Action::Undone
            )
        );
        assert_eq!(
            said(likely),
            (
                Mark::Declined,
                "Episode 5 \u{b7} Frieren",
                "Not recorded: no exact match \u{b7} Likely match \u{b7} mpv",
                Action::Nothing
            )
        );
        assert_eq!(
            said(unmatched),
            (
                Mark::Declined,
                "Episode 3 \u{b7} Dandadan",
                "Not recorded: no exact match \u{b7} mpv",
                Action::Nothing
            )
        );
        assert_eq!(
            said(added.watch.id),
            (
                Mark::Added,
                "Episode 1 \u{b7} Mushishi",
                "Added to library \u{b7} mpv",
                Action::Nothing
            )
        );
        assert_eq!(
            said(refused.watch.id),
            (
                Mark::Declined,
                "Episode 4 \u{b7} Other",
                "Not recorded: not the next episode \u{b7} added to library \u{b7} mpv",
                Action::Nothing
            )
        );
        assert_eq!(
            said(strayed),
            (
                Mark::Undone,
                "Episode 1 \u{b7} Bocchi",
                "Undone, moved to Kagurabachi \u{b7} mpv",
                Action::Undone
            )
        );
        assert_eq!(
            said(corrected),
            (
                Mark::Recorded,
                "Episode 1 \u{b7} Kagurabachi",
                "Recorded 0 \u{2192} 1 \u{b7} moved here \u{b7} mpv",
                Action::Undo(corrected)
            )
        );
        let watch = watches.iter().find(|watch| watch.id == latest).unwrap();
        assert_eq!(row(latest).file.as_deref(), Some("Frieren - 02.mkv"));
        assert_eq!(
            row(latest).time,
            time_of_day(watch.shown_at(), &TimeZone::UTC)
        );
    }

    #[test]
    fn a_recording_says_where_it_moved_the_show_and_an_undone_one_only_what_it_wrote() {
        let moved = |before, after, undone_at| Recorded {
            progress_before: 11,
            progress: 12,
            at: UNIX_EPOCH,
            undone_at,
            status: Some(StatusMove { before, after }),
        };
        assert_eq!(
            recorded_text(&moved(ListStatus::Watching, ListStatus::Completed, None)),
            "Recorded 11 \u{2192} 12 \u{b7} Completed"
        );
        assert_eq!(
            recorded_text(&moved(ListStatus::Rewatching, ListStatus::Completed, None)),
            "Recorded 11 \u{2192} 12 \u{b7} rewatch finished"
        );
        for before in [
            ListStatus::PlanToWatch,
            ListStatus::OnHold,
            ListStatus::Dropped,
        ] {
            assert_eq!(
                recorded_text(&moved(before, ListStatus::Watching, None)),
                "Recorded 11 \u{2192} 12 \u{b7} Watching",
                "{before:?}"
            );
        }
        assert_eq!(
            recorded_text(&moved(
                ListStatus::Watching,
                ListStatus::Completed,
                Some(UNIX_EPOCH)
            )),
            "Recorded 11 \u{2192} 12"
        );
        assert_eq!(
            recorded_text(&Recorded {
                status: None,
                ..moved(ListStatus::Watching, ListStatus::Completed, None)
            }),
            "Recorded 11 \u{2192} 12"
        );
    }

    #[test]
    fn a_rows_detail_carries_the_move_and_an_undone_rows_does_not() {
        let (_tmp, mut store) = open_store();
        let one_episode = NewEntry {
            total: Some(1),
            ..show("Frieren")
        };
        let frieren = store.add(one_episode.clone()).unwrap().id;
        let completed = store
            .record(recording(frieren, 1, "Frieren - 01.mkv"))
            .unwrap()
            .watch
            .id;
        let spice = store
            .add(NewEntry {
                title: "Spice and Wolf".into(),
                ..one_episode
            })
            .unwrap()
            .id;
        let undone = store
            .record(recording(spice, 1, "Spice - 01.mkv"))
            .unwrap()
            .watch
            .id;
        store.undo(undone).unwrap();
        let library = store.entries().unwrap();
        let watches = store.history(None, 10).unwrap().watches;
        let row = |id: HistoryId| {
            watch_row(
                watches.iter().find(|watch| watch.id == id).unwrap(),
                &library,
            )
        };
        assert_eq!(
            row(completed).detail,
            "Recorded 0 \u{2192} 1 \u{b7} Completed \u{b7} mpv"
        );
        let undone = row(undone);
        assert_eq!(
            (undone.mark, undone.detail.as_str(), undone.action),
            (
                Mark::Undone,
                "Recorded 0 \u{2192} 1 \u{b7} mpv",
                Action::Undone
            )
        );
    }

    /// Adds `title` at episode 3, ties it to `media` and reports the list
    /// holding `held` for it, so the library takes the difference and
    /// writes the update row History reads. The app must be signed in.
    fn take_from_list(app: &mut Ryuuji, title: &str, media: u64, held: ListState) -> EntryId {
        app.dispatch(Command::AddEntry(NewEntry {
            progress: 3,
            ..show(title)
        }));
        let id = app
            .state()
            .library
            .iter()
            .find(|entry| entry.title == title)
            .unwrap()
            .id;
        app.dispatch(Command::LinkShow {
            entry: id,
            service: Service::AniList,
            media: Media {
                id: MediaId(media),
                title: title.to_owned(),
                episodes: None,
                mal_id: None,
            },
        });
        let asked = ListState {
            status: ListStatus::Watching,
            progress: 3,
            repeat: 0,
        };
        app.dispatch(Command::Synced(Report::Reconciled {
            service: Service::AniList,
            user: UserId(7),
            entry: id,
            media: MediaId(media),
            asked: Asked {
                wanted: asked,
                base: asked,
            },
            remote: Remote::Listed {
                id: Some(ListEntryId(media)),
                state: held,
            },
        }));
        id
    }

    #[test]
    fn an_update_row_says_what_the_list_changed_and_offers_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = ryuuji_core::DataDir::at(tmp.path()).unwrap();
        let mut app = Ryuuji::open(&dir).unwrap();
        app.dispatch(Command::Synced(Report::SignedIn(Account {
            service: Service::AniList,
            user: UserId(7),
            name: "someone".into(),
            expires: None,
            signed: Signed::In,
        })));
        let held = |status, progress| ListState {
            status,
            progress,
            repeat: 0,
        };
        take_from_list(&mut app, "Frieren", 5, held(ListStatus::Completed, 28));
        take_from_list(&mut app, "Dandadan", 6, held(ListStatus::Watching, 5));
        let status_only = take_from_list(&mut app, "Mushishi", 7, held(ListStatus::OnHold, 3));

        let page = app.history(None, 10).unwrap();
        assert_eq!(page.watches, vec![]);
        let library = app.state().library.clone();
        let rows: Vec<Row> = page
            .updates
            .iter()
            .map(|update| Row::of(Line::Update(update), &library, &TimeZone::UTC))
            .collect();
        let said: Vec<(&str, &str)> = rows
            .iter()
            .map(|row| (row.title.as_str(), row.detail.as_str()))
            .collect();
        assert_eq!(
            said,
            vec![
                ("Mushishi", "Updated from AniList \u{b7} On hold"),
                ("Dandadan", "Updated from AniList 3 \u{2192} 5"),
                (
                    "Frieren",
                    "Updated from AniList 3 \u{2192} 28 \u{b7} Completed"
                ),
            ]
        );
        let newest = &page.updates[0];
        assert_eq!(
            (
                rows[0].id,
                rows[0].mark,
                rows[0].file.as_deref(),
                rows[0].action
            ),
            (
                RowId::Update(newest.id),
                Mark::Updated,
                None,
                Action::Nothing
            )
        );
        assert_eq!(rows[0].time, time_of_day(newest.at, &TimeZone::UTC));
        assert_eq!(
            Row::of(Line::Update(newest), &[], &TimeZone::UTC).title,
            format!("entry {status_only}")
        );

        // With no watch at all, the updates still head a day of their own.
        let listed = items(&page, &library, SystemTime::now(), &TimeZone::UTC);
        assert_eq!(listed.len(), 4);
        assert_eq!(key(&listed[0]), format!("day-update-{}", newest.id));
        assert_eq!(key(&listed[1]), format!("update-{}", newest.id));
    }

    #[test]
    fn a_title_falls_back_to_the_parsed_then_the_raw_title() {
        let (_tmp, mut store) = open_store();
        let no_title = ProposedMatch {
            episode: None,
            ..proposal("[Group] 1080p.mkv", "", 1, Link::Unmatched)
        };
        let fresh = store
            .add_to_library(show("[Group] 1080p.mkv"), None, &no_title)
            .unwrap();
        // The entry is found by id, so the library's title wins.
        assert_eq!(
            row_title(&fresh.watch, &store.entries().unwrap()),
            "[Group] 1080p.mkv"
        );
        assert_eq!(row_title(&fresh.watch, &[]), "[Group] 1080p.mkv");
        let parsed = store
            .decline(
                None,
                &proposal("Show - 07.mkv", "Show", 7, Link::Unmatched),
                Decline::NotExact,
            )
            .unwrap();
        assert_eq!(row_title(&parsed, &[]), "Episode 7 \u{b7} Show");
    }

    #[test]
    fn items_head_each_day_and_end_a_full_page_with_older() {
        let (_tmp, mut store) = open_store();
        let entry = store.add(show("Show")).unwrap().id;
        for episode in 1..=3 {
            store.record(recording(entry, episode, "Show.mkv")).unwrap();
        }
        let library = store.entries().unwrap();
        let now = SystemTime::now();

        let page = store.history(None, 2).unwrap();
        let listed = items(&page, &library, now, &TimeZone::UTC);
        let (first, second) = (page.watches[0].id, page.watches[1].id);
        assert_eq!(
            listed.iter().map(key).collect::<Vec<_>>(),
            vec![
                format!("day-watch-{first}"),
                format!("watch-{first}"),
                format!("watch-{second}"),
                "older".to_owned(),
            ]
        );
        assert!(matches!(&listed[0], Item::Day { label, .. } if label == "Today"));

        let whole = store.history(None, 10).unwrap();
        let listed = items(&whole, &library, now, &TimeZone::UTC);
        assert_eq!(listed.len(), 4);
        assert!(!listed.contains(&Item::Older));
    }
}
