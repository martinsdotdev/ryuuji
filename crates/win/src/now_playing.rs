//! Now playing, as the design draws it. While a file plays: where the viewing
//! stands against its recording, the show on a card with the episode's
//! timeline, what the parser read off the file, and, when the file names no
//! show for sure, the card that asks which show it is. While nothing plays:
//! the shows to pick up again. The caller works out everything from
//! [`AppState`], so the page never carries the library.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, SystemTime};

use ryuuji_core::{
    AppState, Certainty, Command, Described, Detail, ElementKind, EntryId, LastWatched,
    LibraryEntry, Link, ListStatus, NowPlaying, Options, Page, ProposedMatch, Reading,
    RecordOutcome, Ryuuji, WatchProgress, normalize_title, parse,
};
use tracing::warn;
use windows_reactor::*;

use crate::list_pick::{self, ListPick, ListPickProps};
use crate::share;
use crate::show_info;
use crate::standing::{Standing, Tone, Why, standing};
use crate::ui::{
    self, CONTENT_MAX_WIDTH, ICON_FONT, MONO_FONT, age_of, caption, card_frame, clock_text,
    episode_text, placeholder,
};

/// How many shows a library needs before the picker offers a search box.
/// Below this the whole list is on screen, so a field to narrow it would be
/// one more thing to read for nothing.
const SEARCH_FROM: usize = 12;

/// How many shows Continue watching offers.
const CONTINUE: usize = 5;

/// Why a Play link is there but cannot be used.
const PLAY_LATER: &str = "Ryuuji doesn't know where your files are yet.";

const RECORD_GLYPH: &str = "\u{E7C8}";
const CHECK_GLYPH: &str = "\u{E73E}";
const CAUTION_GLYPH: &str = "\u{E7BA}";
const INFO_GLYPH: &str = "\u{E946}";

/// What the card says once a file is ignored. Nothing is being matched,
/// because nothing is meant to be.
const IGNORED_BODY: &str = "Ryuuji won't propose this file again, or any file whose title reads \
                            the same. It keeps playing; nothing else changes.";

/// Builds the page from the state, resolving on the page's behalf what it
/// must not reason about itself. Continue watching is read from the store
/// only while nothing plays, which is the only time it is shown.
pub(crate) fn body(
    state: &AppState,
    dispatch: Dispatch<Command>,
    detection_down: bool,
    core: &Rc<RefCell<Ryuuji>>,
) -> Element {
    let recent = if matches!(state.now_playing, NowPlaying::Idle) {
        core.borrow()
            .last_watched(CONTINUE * 2)
            .unwrap_or_else(|err| {
                warn!(error = %err, "shows watched last not read; Continue watching is empty");
                Vec::new()
            })
    } else {
        Vec::new()
    };
    let key = state
        .last_match
        .as_ref()
        .map_or_else(String::new, |m| m.raw_title.clone());
    let settled = settled_entry(state);
    component(
        now_playing,
        NowPlayingProps {
            dispatch,
            view: view(state, detection_down, &recent, SystemTime::now()),
            question: question(state),
            list_pick: settled.and_then(|entry| list_pick::needed(state, entry)),
            described: show_info::source(state, core, settled),
        },
    )
    // A new file starts with the picker closed and its search empty.
    .with_key(key)
}

/// The show a playing file is settled on, when it is: a pick on a list is
/// only asked of a show the file surely is.
fn settled_entry(state: &AppState) -> Option<EntryId> {
    if !matches!(state.now_playing, NowPlaying::Playing { .. }) {
        return None;
    }
    let m = state.last_match.as_ref()?;
    is_settled(state, m).then(|| m.link.entry()).flatten()
}

/// Whether a file is settled on a show: matched for sure, and not ignored.
/// Anything short of that leaves the person to say which show it is.
fn is_settled(state: &AppState, m: &ProposedMatch) -> bool {
    !state.ignored && matches!(m.link, Link::Exact(_))
}

/// One library row the picker can offer, worked out by the caller so the
/// page still never reasons about the library itself.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ShowChoice {
    entry: EntryId,
    title: String,
    /// `Watching · 7 / 28`, or `Watching · 7` on a show whose length is
    /// unknown.
    line: String,
}

/// Every show that could be picked, by title, so the order does not shift
/// under the person as their progress changes.
fn choices(library: &[LibraryEntry]) -> Vec<ShowChoice> {
    let mut choices: Vec<ShowChoice> = library
        .iter()
        .map(|entry| ShowChoice {
            entry: entry.id,
            title: entry.title.clone(),
            line: format!(
                "{} \u{b7} {}",
                ListStatus::of(entry).label(),
                count_text(entry)
            ),
        })
        .collect();
    choices.sort_by(|a, b| a.title.cmp(&b.title));
    choices
}

/// `7 / 28`, or `7` on a show whose length is unknown.
fn count_text(entry: &LibraryEntry) -> String {
    match entry.total {
        Some(total) => format!("{} / {total}", entry.progress),
        None => entry.progress.to_string(),
    }
}

/// What the page shows.
#[derive(Clone, Debug, PartialEq)]
enum View {
    Idle(Idle),
    Detecting,
    Playing(Box<Playing>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Idle {
    detection_down: bool,
    next_up: Vec<NextUp>,
    /// The file last seen, while it still waits for the person to say
    /// which show it is: "Last seen in mpv · 2 min ago".
    last_seen: Option<String>,
}

/// One show Continue watching offers.
#[derive(Clone, Debug, PartialEq, Eq)]
struct NextUp {
    entry: EntryId,
    title: String,
    /// `Episode 8 is next · 7 / 28 · last watched 2 d ago in mpv`.
    line: String,
    next: u32,
}

#[derive(Clone, Debug, PartialEq)]
struct Playing {
    standing: Option<Standing>,
    /// Whether the header offers Undo.
    undo: bool,
    hero: Hero,
    timeline: Option<Timeline>,
    /// What the parser read off the file, then the player.
    file: Vec<(String, String)>,
    raw_title: String,
}

/// The show on its card.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Hero {
    /// `Watching · 7 of 28 episodes`, or `Not in your library`.
    status: String,
    title: String,
    /// `Episode 8`.
    episode: Option<String>,
    /// `A guess: 07 may be a word of the title`.
    doubt: Option<String>,
    /// The show the file is settled on: Edit opens its page, Share names
    /// it, and since two titles can fold alike, the card offers the way to
    /// say it is the wrong show.
    entry: Option<EntryId>,
    /// The episode after this file, while the show has one.
    next: Option<u32>,
}

/// The episode as a bar, with the point where it records.
#[derive(Clone, Debug, PartialEq)]
struct Timeline {
    /// How far through the episode the player is, from 0 to 1.
    value: f64,
    /// Where the tick sits, from 0 to 1, when there is a point to mark.
    mark: Option<f64>,
    start: String,
    /// `records at 12:00`, under the tick, when there is something to say.
    middle: Option<String>,
    end: String,
    /// A viewing on its way to a recording, whose point is said in the
    /// primary text colour rather than the secondary.
    recording: bool,
}

/// What the card that asks which show a file is needs, when it has a
/// question to ask.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Question {
    /// The file is ignored: the card says so and offers the way back.
    ignored: bool,
    /// The show the matcher guessed, listed first and marked.
    likely: Option<EntryId>,
    /// Whether the card opens by itself. A settled match opens it only when
    /// the person says the show is wrong.
    open: bool,
    /// Nothing matched, so the file can be added as a show of its own.
    can_add: bool,
    shows: Vec<ShowChoice>,
}

fn question(state: &AppState) -> Option<Question> {
    let m = state.last_match.as_ref()?;
    Some(Question {
        ignored: state.ignored,
        likely: match m.link {
            Link::Likely(entry) => Some(entry),
            Link::Exact(_) | Link::Unmatched => None,
        },
        open: !is_settled(state, m),
        can_add: m.link == Link::Unmatched,
        shows: choices(&state.library),
    })
}

fn view(state: &AppState, detection_down: bool, recent: &[LastWatched], now: SystemTime) -> View {
    match &state.now_playing {
        NowPlaying::Idle => View::Idle(Idle {
            detection_down,
            next_up: next_up(&state.library, recent, now),
            last_seen: state
                .last_match
                .as_ref()
                .filter(|m| !is_settled(state, m))
                .map(|m| format!("Last seen in {} \u{b7} {}", m.player, age_of(m.at, now))),
        }),
        NowPlaying::Detecting => View::Detecting,
        NowPlaying::Playing {
            title,
            player,
            position,
            duration,
            ..
        } => {
            let standing = standing(state);
            let m = state.last_match.as_ref();
            let entry = m
                .and_then(|m| m.link.entry())
                .and_then(|id| state.entry(id));
            let reading = m.map(|m| parse(&m.raw_title, &Options::default()));
            let mut file = reading.as_ref().map(fact_rows).unwrap_or_default();
            file.push(("Player".to_owned(), player.clone()));
            let show = match (entry, m) {
                (Some(entry), _) => entry.title.clone(),
                (None, Some(m)) => m.shown_title().to_owned(),
                (None, None) => title.clone(),
            };
            let settled = m.is_some_and(|m| is_settled(state, m));
            View::Playing(Box::new(Playing {
                undo: undo_applies(state),
                hero: Hero {
                    status: entry
                        .filter(|_| !state.ignored)
                        .map_or_else(|| "Not in your library".to_owned(), show_info::status_line),
                    entry: entry.filter(|_| settled).map(|entry| entry.id),
                    title: show,
                    episode: m.and_then(|m| m.episode.as_ref()).map(episode_text),
                    doubt: reading.as_ref().and_then(episode_doubt),
                    next: m.and_then(|m| next_after(m, entry)),
                },
                timeline: timeline(
                    *position,
                    *duration,
                    state.watch_progress.as_ref(),
                    standing.as_ref(),
                ),
                standing,
                file,
                raw_title: title.clone(),
            }))
        }
    }
}

/// `Watching Frieren: Beyond Journey's End, episode 8`: what Share hands
/// the share sheet.
fn share_text(show: &str, episode: Option<&str>) -> String {
    match episode {
        Some(episode) => format!("Watching {show}, {}", episode.to_lowercase()),
        None => format!("Watching {show}"),
    }
}

/// The episode after the one this file carries, while the show has one.
fn next_after(m: &ProposedMatch, entry: Option<&LibraryEntry>) -> Option<u32> {
    let entry = entry?;
    let last = *m.episode.as_ref()?.end();
    if entry.total.is_some_and(|total| last >= total) {
        return None;
    }
    last.checked_add(1)
}

/// The shows Continue watching offers: the ones watched last that are still
/// under way and have an episode left, newest first.
fn next_up(library: &[LibraryEntry], recent: &[LastWatched], now: SystemTime) -> Vec<NextUp> {
    recent
        .iter()
        .filter_map(|last| {
            let entry = library.iter().find(|entry| entry.id == last.entry)?;
            if !matches!(
                ListStatus::of(entry),
                ListStatus::Watching | ListStatus::Rewatching
            ) {
                return None;
            }
            let next = entry.progress.checked_add(1)?;
            if entry.total.is_some_and(|total| next > total) {
                return None;
            }
            Some(NextUp {
                entry: entry.id,
                title: entry.title.clone(),
                line: format!(
                    "Episode {next} is next \u{b7} {} \u{b7} last watched {} in {}",
                    count_text(entry),
                    age_of(last.at, now),
                    last.player
                ),
                next,
            })
        })
        .take(CONTINUE)
        .collect()
}

/// The episode as a bar. The tick marks where the recording lands if the
/// episode is watched straight through from here, since the count is of
/// time watched rather than of the position: it moves with a seek.
fn timeline(
    position: Duration,
    duration: Duration,
    progress: Option<&WatchProgress>,
    standing: Option<&Standing>,
) -> Option<Timeline> {
    if duration.is_zero() {
        return None;
    }
    let fraction = |at: Duration| (at.as_secs_f64() / duration.as_secs_f64()).clamp(0.0, 1.0);
    let lands = progress
        .map(|progress| position + progress.threshold.saturating_sub(progress.accrued))
        .filter(|at| *at <= duration);
    // Where the recording lands, with what it still waits on.
    let point = |waits_on: &str| match lands {
        Some(at) => (
            Some(fraction(at)),
            Some(format!("records at {}{waits_on}", clock_text(at))),
        ),
        None => (None, None),
    };
    let ((mark, middle), recording) = match standing {
        Some(Standing::Counting { .. }) => (point(""), true),
        Some(Standing::NotRecording {
            why: Why::NoShow,
            refused: false,
        }) => (point(" once linked"), false),
        Some(Standing::NotRecording {
            why: Why::Unconfirmed,
            refused: false,
        }) => (point(" once confirmed"), false),
        Some(Standing::Recorded { .. }) => ((None, Some("recorded".to_owned())), true),
        Some(Standing::Undone { .. } | Standing::NotRecording { .. }) => ((None, None), false),
        None => ((None, None), true),
    };
    Some(Timeline {
        value: fraction(position),
        mark,
        start: clock_text(position),
        middle,
        end: clock_text(duration),
        recording,
    })
}

/// Whether Now playing may offer Undo: the viewing recorded, and its show
/// still stands at the last episode the file carries, which is the progress
/// the recording wrote. The store refuses any other undo, and History offers
/// Undo by the same rule.
fn undo_applies(state: &AppState) -> bool {
    let recorded = matches!(
        state.watch_progress,
        Some(WatchProgress {
            outcome: RecordOutcome::Recorded(_),
            ..
        })
    );
    let Some(last) = state.last_match.as_ref().filter(|_| recorded) else {
        return false;
    };
    let (Some(entry), Some(episode)) = (last.link.entry(), last.episode.as_ref()) else {
        return false;
    };
    state
        .entry(entry)
        .is_some_and(|shown| shown.progress == *episode.end())
}

/// What Now playing is handed. `dispatch` is fresh every render and compares
/// by identity, so it comes first and the props compare stops there.
#[derive(Clone, Debug, PartialEq)]
struct NowPlayingProps {
    dispatch: Dispatch<Command>,
    view: View,
    question: Option<Question>,
    /// The playing show's entry on a list, when the person has to pick it.
    list_pick: Option<ListPick>,
    /// Where to read what the first service the playing show is paired
    /// with says about it: the cover, the synopsis and the details.
    described: show_info::Source,
}

fn now_playing(props: &NowPlayingProps, cx: &mut RenderCx) -> Element {
    let (wrong_show, set_wrong_show) = cx.use_state(false);
    let (query, set_query) = cx.use_state(String::new());
    let idle = match &props.view {
        View::Idle(idle) => Some(idle),
        View::Detecting | View::Playing(_) => None,
    };
    ui::use_refresh(
        cx,
        idle.is_some_and(|idle| !idle.next_up.is_empty() || idle.last_seen.is_some()),
    );
    let described = show_info::use_described(cx, &props.described);

    let asking = props
        .question
        .as_ref()
        .filter(|question| question.open || wrong_show);
    let picker = asking.map(|question| {
        let cancel = (!question.open).then(|| {
            let set_wrong_show = set_wrong_show.clone();
            let set_query = set_query.clone();
            move || {
                set_wrong_show.call(false);
                set_query.call(String::new());
            }
        });
        which_show(question, &props.dispatch, &query, set_query.clone(), cancel)
    });

    let sections: Vec<Element> = match &props.view {
        View::Detecting => vec![placeholder(
            "Detecting\u{2026}",
            "A player is open; waiting for something recognisable.",
        )],
        View::Idle(idle) => idle_sections(idle, picker, &props.dispatch),
        View::Playing(playing) => {
            // A show the person says is wrong is settled before its list
            // entry, which belongs to whichever show they pick.
            let card = if wrong_show {
                picker
            } else {
                let list_pick = props.list_pick.clone().map(|pick| {
                    list_pick::card(ListPickProps {
                        dispatch: props.dispatch.clone(),
                        pick,
                        cancel: false,
                    })
                });
                list_pick.or(picker)
            };
            playing_sections(
                playing,
                card,
                &props.dispatch,
                set_wrong_show,
                described.as_ref(),
            )
        }
    };
    scroll_viewer(vstack(sections).spacing(16.0).max_width(CONTENT_MAX_WIDTH)).into()
}

fn playing_sections(
    playing: &Playing,
    picker: Option<Element>,
    dispatch: &Dispatch<Command>,
    set_wrong_show: SetState<bool>,
    described: Option<&Described>,
) -> Vec<Element> {
    let mut sections = Vec::new();
    if let Some(standing) = &playing.standing {
        sections.push(header(standing, playing.undo, dispatch));
    }
    sections.push(hero(
        &playing.hero,
        playing.timeline.as_ref(),
        dispatch,
        set_wrong_show,
        described,
    ));
    if let Some(picker) = picker {
        sections.push(picker);
    }
    let info = described.map(|described| &described.info);
    if let Some(synopsis) = info.and_then(show_info::synopsis) {
        sections.push(synopsis);
    }
    let file = this_file(&playing.file, &playing.raw_title);
    // The design sets the details and the file side by side, the details
    // the wider.
    sections.push(
        match info.and_then(|info| show_info::details(info, &playing.hero.title)) {
            Some(details) => grid((border(details).grid_column(0), border(file).grid_column(1)))
                .columns([GridLength::Star(3.0), GridLength::Star(2.0)])
                .column_spacing(8.0)
                .into(),
            None => file,
        },
    );
    sections
}

/// The viewing's standing, right-aligned under the page's title: the
/// toolkit's header takes only text, so it cannot sit beside it.
fn header(standing: &Standing, undo: bool, dispatch: &Dispatch<Command>) -> Element {
    let (glyph, brush) = match standing.tone() {
        Tone::Accent => (RECORD_GLYPH, ThemeRef::Accent),
        Tone::Success => (CHECK_GLYPH, ThemeRef::SystemSuccess),
        Tone::Neutral => (INFO_GLYPH, ThemeRef::SecondaryText),
        Tone::Caution => (CAUTION_GLYPH, ThemeRef::SystemCaution),
    };
    let mut parts: Vec<Element> = vec![
        text_block(glyph)
            .font_family(ICON_FONT)
            .font_size(16.0)
            .foreground(brush)
            .vertical_alignment(VerticalAlignment::Center)
            .into(),
        text_block(standing.header())
            .semibold()
            .vertical_alignment(VerticalAlignment::Center)
            .into(),
    ];
    let action = match standing {
        Standing::Counting { .. } => Some(("Don't record", Command::DontRecord)),
        Standing::Recorded { watch, .. } if undo => Some(("Undo", Command::UndoRecording(*watch))),
        Standing::Recorded { .. } | Standing::Undone { .. } | Standing::NotRecording { .. } => None,
    };
    if let Some((label, command)) = action {
        parts.push(
            button(label)
                .on_click(ui::send(dispatch, command))
                .vertical_alignment(VerticalAlignment::Center)
                .into(),
        );
    }
    hstack(parts)
        .spacing(10.0)
        .horizontal_alignment(HorizontalAlignment::Right)
        .into()
}

fn hero(
    hero: &Hero,
    timeline: Option<&Timeline>,
    dispatch: &Dispatch<Command>,
    set_wrong_show: SetState<bool>,
    described: Option<&Described>,
) -> Element {
    let mut lines: Vec<Element> = vec![
        caption(hero.status.clone()).into(),
        text_block(hero.title.clone())
            .font_size(28.0)
            .semibold()
            .wrap()
            .into(),
    ];
    if let Some(episode) = &hero.episode {
        lines.push(
            text_block(episode.clone())
                .font_size(20.0)
                .semibold()
                .into(),
        );
    }
    if let Some(doubt) = &hero.doubt {
        lines.push(caption(doubt.clone()).wrap().into());
    }
    let mut links: Vec<Element> = Vec::new();
    if let Some(entry) = hero.entry {
        links.push(
            button("Edit")
                .text_link()
                .on_click(ui::send(
                    dispatch,
                    Command::OpenDetail(Detail::Entry(entry)),
                ))
                .into(),
        );
    }
    if hero.entry.is_some() {
        let show = hero.title.clone();
        let text = share_text(&show, hero.episode.as_deref());
        links.push(
            button("Share")
                .text_link()
                .on_click(move || {
                    if let Err(err) = share::share(&show, &text) {
                        warn!(error = %err, "share sheet not opened");
                    }
                })
                .into(),
        );
    }
    if let Some(next) = hero.next {
        links.push(
            button(format!("Play episode {next}"))
                .text_link()
                .enabled(false)
                .tooltip(PLAY_LATER)
                .into(),
        );
    }
    if hero.entry.is_some() {
        links.push(
            button("Wrong show?")
                .text_link()
                .on_click(move || set_wrong_show.call(true))
                .into(),
        );
    }
    if !links.is_empty() {
        lines.push(dotted(links));
    }
    let mut card: Vec<Element> = vec![vstack(lines).spacing(4.0).into()];
    if let Some(timeline) = timeline {
        card.push(timeline_element(timeline));
    }
    card_frame(show_info::beside_cover(
        described,
        vstack(card)
            .spacing(16.0)
            .vertical_alignment(VerticalAlignment::Bottom),
    ))
    .into()
}

/// Links in a row with a dot between each, the way the design separates
/// them.
fn dotted(links: Vec<Element>) -> Element {
    let mut row: Vec<Element> = Vec::new();
    for link in links {
        if !row.is_empty() {
            row.push(
                caption("\u{b7}")
                    .vertical_alignment(VerticalAlignment::Center)
                    .into(),
            );
        }
        row.push(link);
    }
    hstack(row).spacing(4.0).into()
}

/// The bar, the tick under it, and the times under that. The tick and its
/// words share one column centred on the mark, so the words sit under the
/// tick wherever it falls; the start and the end are pinned to the edges.
fn timeline_element(timeline: &Timeline) -> Element {
    let bar = ProgressBar::new(timeline.value * 100.0);
    let centre = timeline.mark.unwrap_or(0.5);
    let mut cells: Vec<Element> = vec![
        bar.vertical_alignment(VerticalAlignment::Center)
            .grid_row(0)
            .grid_column(0)
            .grid_column_span(3)
            .into(),
        caption(timeline.start.clone())
            .grid_row(4)
            .grid_column(0)
            .grid_column_span(3)
            .horizontal_alignment(HorizontalAlignment::Left)
            .into(),
        caption(timeline.end.clone())
            .grid_row(4)
            .grid_column(0)
            .grid_column_span(3)
            .horizontal_alignment(HorizontalAlignment::Right)
            .into(),
    ];
    if timeline.mark.is_some() {
        cells.push(
            border(Element::Empty)
                .width(1.0)
                .height(4.0)
                .background(ThemeRef::custom(ui::STRONG_FILL))
                .grid_row(2)
                .grid_column(1)
                .horizontal_alignment(HorizontalAlignment::Center)
                .into(),
        );
    }
    if let Some(middle) = &timeline.middle {
        let words = caption(middle.clone()).max_lines(1);
        let words = if timeline.mark.is_some() && timeline.recording {
            words.foreground(ThemeRef::PrimaryText)
        } else {
            words
        };
        cells.push(
            words
                .grid_row(4)
                .grid_column(1)
                .horizontal_alignment(HorizontalAlignment::Center)
                .into(),
        );
    }
    grid(cells)
        .columns(mark_columns(centre))
        .rows([
            GridLength::Pixel(3.0),
            GridLength::Pixel(2.0),
            GridLength::Pixel(4.0),
            GridLength::Pixel(3.0),
            GridLength::Auto,
        ])
        .into()
}

/// Three columns whose middle one is centred on `mark`: as wide as the
/// nearer edge allows, so whatever is centred in it is centred on the mark.
fn mark_columns(mark: f64) -> [GridLength; 3] {
    let half = mark.min(1.0 - mark);
    [star(mark - half), star(2.0 * half), star(1.0 - mark - half)]
}

fn star(weight: f64) -> GridLength {
    if weight > 0.0 {
        GridLength::Star(weight)
    } else {
        GridLength::Pixel(0.0)
    }
}

fn this_file(rows: &[(String, String)], raw_title: &str) -> Element {
    let mut children: Vec<Element> = vec![text_block("This file").semibold().into()];
    if !rows.is_empty() {
        children.push(ui::facts_table(rows.iter().cloned()).into());
    }
    children.push(
        caption(raw_title)
            .font_family(MONO_FONT)
            .wrap()
            .selectable()
            .into(),
    );
    card_frame(vstack(children).spacing(12.0)).into()
}

/// The card that asks which show a file is: the library's shows to link it
/// to, the guessed one first, and the other ways out. An ignored file is
/// answered first, since it reads as unmatched and would otherwise be
/// offered to the library it was told to stay out of.
fn which_show(
    question: &Question,
    dispatch: &Dispatch<Command>,
    query: &str,
    set_query: SetState<String>,
    cancel: Option<impl Fn() + Clone + 'static>,
) -> Element {
    if question.ignored {
        return card_frame(
            vstack((
                text_block("You're ignoring this file").semibold(),
                caption(IGNORED_BODY).wrap(),
                dispatching(dispatch, "Stop ignoring", Command::StopIgnoring),
            ))
            .spacing(8.0),
        )
        .into();
    }
    let mut children: Vec<Element> = vec![text_block("Which show is this?").semibold().into()];
    if question.shows.len() >= SEARCH_FROM {
        children.push(
            auto_suggest_box(query.to_owned())
                .placeholder_text("Search your library")
                .on_text_changed(set_query)
                .into(),
        );
    }
    let offers = offers(&question.shows, question.likely, query);
    if offers.is_empty() {
        children.push(caption("No show in your library has that title.").into());
    } else {
        let rows = offers.len();
        let dispatch_row = dispatch.clone();
        let close = cancel.clone();
        let list = list_view(offers, move |offer: &ShowOffer, _| {
            offer_row(offer, &dispatch_row, close.clone())
        })
        .with_key_selector(|offer: &ShowOffer| offer.show.entry.to_string())
        .selection_mode(SelectionMode::None);
        children.push(ui::picker_list(list, rows));
    }
    let mut buttons: Vec<Element> = Vec::new();
    if question.can_add {
        buttons.push(dispatching(
            dispatch,
            "Add as new show",
            Command::AddProposedToLibrary,
        ));
    }
    if let Some(cancel) = cancel {
        buttons.push(button("Cancel").on_click(cancel).into());
    } else {
        buttons.push(dispatching(
            dispatch,
            "Ignore this file",
            Command::IgnoreFile,
        ));
    }
    children.push(hstack(buttons).spacing(8.0).into());
    card_frame(vstack(children).spacing(12.0)).into()
}

/// One show the picker offers, and whether it is the matcher's guess.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ShowOffer {
    show: ShowChoice,
    likely: bool,
}

/// The shows a query narrows to, the guessed one first. Folded the way
/// matching compares titles, so `frieren beyond journeys end` finds
/// `Frieren: Beyond Journey's End`; an empty query keeps every show.
fn offers(shows: &[ShowChoice], likely: Option<EntryId>, query: &str) -> Vec<ShowOffer> {
    let mut offers: Vec<ShowOffer> = matching_shows(shows, query)
        .into_iter()
        .map(|show| ShowOffer {
            likely: Some(show.entry) == likely,
            show,
        })
        .collect();
    offers.sort_by_key(|offer| !offer.likely);
    offers
}

/// One show to link the file to. `close` closes a card the person opened
/// by hand, once the link is made.
fn offer_row(
    offer: &ShowOffer,
    dispatch: &Dispatch<Command>,
    close: Option<impl Fn() + 'static>,
) -> Grid {
    let mut name: Vec<Element> = vec![text_block(offer.show.title.clone()).wrap().into()];
    let mut line: Vec<Element> = vec![caption(offer.show.line.clone()).into()];
    if offer.likely {
        line.push(caption("Likely").foreground(ThemeRef::AccentText).into());
    }
    name.push(hstack(line).spacing(8.0).into());
    let entry = offer.show.entry;
    let dispatch = dispatch.clone();
    grid((
        vstack(name)
            .spacing(2.0)
            .vertical_alignment(VerticalAlignment::Center)
            .grid_column(0),
        button("Link")
            .on_click(move || {
                dispatch.call(Command::PickShow(entry));
                if let Some(close) = &close {
                    close();
                }
            })
            .automation_name(format!("Link to {}", row_label(&offer.show)))
            .vertical_alignment(VerticalAlignment::Center)
            .grid_column(1),
    ))
    .columns([GridLength::STAR, GridLength::Auto])
}

/// What a picker row announces: the show and where it stands, in the order
/// the row reads, rather than the row's position.
fn row_label(show: &ShowChoice) -> String {
    format!("{}, {}", show.title, show.line)
}

fn matching_shows(shows: &[ShowChoice], query: &str) -> Vec<ShowChoice> {
    let needle = normalize_title(query);
    shows
        .iter()
        .filter(|show| normalize_title(&show.title).contains(&needle))
        .cloned()
        .collect()
}

/// A button that hands one command to the core and nothing else.
fn dispatching(dispatch: &Dispatch<Command>, label: &str, command: Command) -> Element {
    button(label).on_click(ui::send(dispatch, command)).into()
}

fn idle_sections(
    idle: &Idle,
    picker: Option<Element>,
    dispatch: &Dispatch<Command>,
) -> Vec<Element> {
    let (heading, body) = idle_placeholder_copy(idle.detection_down);
    let mut sections = vec![placeholder(heading, body)];
    if let (Some(last_seen), Some(picker)) = (&idle.last_seen, picker) {
        sections.push(
            vstack((caption(last_seen.clone()), picker))
                .spacing(4.0)
                .into(),
        );
    }
    let mut watching: Vec<Element> = vec![
        grid((
            text_block("Continue watching")
                .semibold()
                .vertical_alignment(VerticalAlignment::Center)
                .grid_column(0),
            button("Open Library")
                .text_link()
                .on_click(ui::send(dispatch, Command::SelectPage(Page::Library)))
                .grid_column(1),
        ))
        .columns([GridLength::STAR, GridLength::Auto])
        .into(),
    ];
    if idle.next_up.is_empty() {
        watching
            .push(caption("Shows you're watching appear here once an episode is recorded.").into());
    }
    for show in &idle.next_up {
        watching.push(
            card_frame(
                grid((
                    vstack((
                        text_block(show.title.clone()).wrap(),
                        caption(show.line.clone()).wrap(),
                    ))
                    .spacing(2.0)
                    .vertical_alignment(VerticalAlignment::Center)
                    .grid_column(0),
                    button(format!("Play episode {}", show.next))
                        .enabled(false)
                        .tooltip(PLAY_LATER)
                        .vertical_alignment(VerticalAlignment::Center)
                        .grid_column(1),
                ))
                .columns([GridLength::STAR, GridLength::Auto])
                .column_spacing(12.0),
            )
            .into(),
        );
    }
    sections.push(vstack(watching).spacing(8.0).into());
    sections
}

/// The Now playing placeholder copy: the plain idle pair, or the
/// detection-down pair when the watcher never started.
fn idle_placeholder_copy(detection_down: bool) -> (&'static str, &'static str) {
    if detection_down {
        (
            "Detection isn't running",
            "Ryuuji can't watch your players right now. See Diagnostics in Settings for what went wrong.",
        )
    } else {
        (
            "Nothing is playing",
            "Open an episode in your player and it will show up here.",
        )
    }
}

/// `A guess: 07 may be a word of the title`, when the parser had to guess
/// the episode. The card already names the episode, so this says only what
/// else its text could have been. Read off the raw title like the file's
/// facts, so nothing about the doubt is carried on the proposal.
fn episode_doubt(reading: &Reading) -> Option<String> {
    if reading.episodes()?.certainty != Certainty::Guessed {
        return None;
    }
    let doubts: Vec<String> = reading
        .alternatives()
        .iter()
        .filter(|alternative| alternative.taken.kind == Some(ElementKind::EpisodeNumber))
        .map(|alternative| {
            format!(
                "{} may be {}",
                alternative.text,
                alternative.passed_description()
            )
        })
        .collect();
    (!doubts.is_empty()).then(|| format!("A guess: {}", doubts.join("; ")))
}

/// Every element the parser finds in the raw player title. Read here
/// rather than carried on the proposal, so a reloaded one reads the same.
fn fact_rows(reading: &Reading) -> Vec<(String, String)> {
    reading
        .elements()
        .iter()
        .map(|(kind, value)| (fact_label(kind.label()), value.to_owned()))
        .collect()
}

fn fact_label(label: &str) -> String {
    let mut label = label.replace('_', " ");
    if let Some(first) = label.get_mut(..1) {
        first.make_ascii_uppercase();
    }
    label
}

#[cfg(test)]
mod tests {
    use ryuuji_core::{DataDir, NewEntry, NewRecording, Opened, Store, WatchStatus};

    use super::*;

    fn secs(value: u64) -> Duration {
        Duration::from_secs(value)
    }

    fn proposal() -> ProposedMatch {
        ProposedMatch {
            raw_title: "raw.mkv".to_owned(),
            parsed_title: "Parsed".to_owned(),
            episode: None,
            season: None,
            release_group: None,
            link: Link::Unmatched,
            player: "mpv".to_owned(),
            at: SystemTime::UNIX_EPOCH,
        }
    }

    /// Two shows with ids of their own, which only a store mints: Frieren
    /// at 7 of 28, and Vinland Saga at 2 with its length unknown.
    fn two_shows() -> (LibraryEntry, LibraryEntry) {
        let tmp = tempfile::tempdir().unwrap();
        let dir = DataDir::at(tmp.path()).unwrap();
        let Opened { mut store, .. } = Store::open(&dir).unwrap();
        let add = |store: &mut Store, title: &str, progress, total| {
            store
                .add(NewEntry {
                    title: title.to_owned(),
                    status: WatchStatus::Watching,
                    progress,
                    total,
                    rewatching: false,
                })
                .unwrap()
        };
        let first = add(&mut store, "Frieren: Beyond Journey's End", 7, Some(28));
        let second = add(&mut store, "Vinland Saga Season 2", 2, None);
        (first, second)
    }

    fn counting(accrued: u64) -> WatchProgress {
        WatchProgress {
            accrued: secs(accrued),
            threshold: secs(720),
            outcome: RecordOutcome::Counting,
        }
    }

    fn standing_of(episode: u32) -> Standing {
        Standing::Counting {
            remaining: secs(27),
            episode: Some(episode..=episode),
        }
    }

    fn playing(title: &str) -> NowPlaying {
        NowPlaying::Playing {
            title: title.to_owned(),
            player: "mpv".to_owned(),
            status: ryuuji_core::PlaybackStatus::Playing,
            position: secs(693),
            duration: secs(1440),
        }
    }

    /// A state with `show` in the library and a file standing against it.
    fn standing_against(show: &LibraryEntry, now_playing: NowPlaying, link: Link) -> AppState {
        AppState {
            library: vec![show.clone()],
            now_playing,
            last_match: Some(ProposedMatch {
                raw_title: "Frieren - 08.mkv".to_owned(),
                parsed_title: "Frieren".to_owned(),
                episode: Some(8..=8),
                link,
                ..proposal()
            }),
            ..AppState::default()
        }
    }

    fn hero_of(view: View) -> Hero {
        match view {
            View::Playing(playing) => playing.hero,
            other => panic!("not playing: {other:?}"),
        }
    }

    /// The card names the show and offers its page only once the file is
    /// settled on it. A guess names the show it guessed and offers nothing,
    /// and an ignored file is not in the library whatever it matched.
    #[test]
    fn the_card_offers_the_show_only_once_the_file_is_settled_on_it() {
        let (frieren, _) = two_shows();
        let card = |link, ignored| {
            let state = AppState {
                ignored,
                ..standing_against(&frieren, playing("Frieren - 08.mkv"), link)
            };
            hero_of(view(&state, false, &[], SystemTime::UNIX_EPOCH))
        };
        let settled = card(Link::Exact(frieren.id), false);
        assert_eq!(settled.title, "Frieren: Beyond Journey's End");
        assert_eq!(settled.status, "Watching \u{b7} 7 of 28 episodes");
        assert_eq!(settled.episode.as_deref(), Some("Episode 8"));
        assert_eq!(settled.entry, Some(frieren.id));

        let guess = card(Link::Likely(frieren.id), false);
        assert_eq!(guess.title, "Frieren: Beyond Journey's End");
        assert_eq!(guess.entry, None);

        let ignored = card(Link::Exact(frieren.id), true);
        assert_eq!(ignored.status, "Not in your library");
        assert_eq!(ignored.entry, None);

        let unmatched = card(Link::Unmatched, false);
        assert_eq!(unmatched.status, "Not in your library");
        assert_eq!(unmatched.title, "Frieren");
        assert_eq!(unmatched.entry, None);
    }

    /// Idle, the page says which file still waits for an answer, and only
    /// that one: a file settled on its show has nothing left to ask.
    #[test]
    fn idle_says_what_was_last_seen_only_while_it_still_needs_an_answer() {
        let (frieren, _) = two_shows();
        let now = SystemTime::UNIX_EPOCH + secs(120);
        let idle = |link| match view(
            &standing_against(&frieren, NowPlaying::Idle, link),
            false,
            &[],
            now,
        ) {
            View::Idle(idle) => idle,
            other => panic!("not idle: {other:?}"),
        };
        assert_eq!(
            idle(Link::Unmatched).last_seen.as_deref(),
            Some("Last seen in mpv \u{b7} 2 min ago")
        );
        assert_eq!(idle(Link::Exact(frieren.id)).last_seen, None);
        let detecting = AppState {
            now_playing: NowPlaying::Detecting,
            ..AppState::default()
        };
        assert_eq!(view(&detecting, false, &[], now), View::Detecting);
    }

    /// A pick on a list is asked only of a show the playing file surely is,
    /// and only while it plays: a settled match left standing after the
    /// player stopped asks nothing.
    #[test]
    fn a_list_pick_is_asked_only_while_a_settled_file_plays() {
        let (frieren, _) = two_shows();
        let exact = Link::Exact(frieren.id);
        let state = |now_playing, link| standing_against(&frieren, now_playing, link);
        assert_eq!(
            settled_entry(&state(playing("Frieren - 08.mkv"), exact)),
            Some(frieren.id)
        );
        assert_eq!(settled_entry(&state(NowPlaying::Idle, exact)), None);
        assert_eq!(
            settled_entry(&state(
                playing("Frieren - 08.mkv"),
                Link::Likely(frieren.id)
            )),
            None
        );
        let ignored = AppState {
            ignored: true,
            ..state(playing("Frieren - 08.mkv"), exact)
        };
        assert_eq!(settled_entry(&ignored), None);
    }

    #[test]
    fn the_tick_marks_where_the_recording_lands_if_watched_straight_through() {
        let line = timeline(
            secs(693),
            secs(1440),
            Some(&counting(693)),
            Some(&standing_of(8)),
        )
        .unwrap();
        assert_eq!(line.start, "11:33");
        assert_eq!(line.end, "24:00");
        assert_eq!(line.middle.as_deref(), Some("records at 12:00"));
        assert_eq!(line.mark, Some(0.5));
        assert!((line.value - 693.0 / 1440.0).abs() < 1e-9);
        assert!(line.recording);

        // A seek ahead moved the player but not the count, so the write
        // lands later than half the episode.
        let seeked = timeline(
            secs(900),
            secs(1440),
            Some(&counting(300)),
            Some(&standing_of(8)),
        )
        .unwrap();
        assert_eq!(seeked.middle.as_deref(), Some("records at 22:00"));
        assert_eq!(seeked.mark, Some(1320.0 / 1440.0));

        // Too little watched to record before the episode ends.
        let short = timeline(
            secs(1300),
            secs(1440),
            Some(&counting(100)),
            Some(&standing_of(8)),
        )
        .unwrap();
        assert_eq!((short.mark, short.middle), (None, None));
    }

    #[test]
    fn a_file_waiting_for_its_show_says_what_recording_waits_on() {
        let unmatched = Standing::NotRecording {
            why: Why::NoShow,
            refused: false,
        };
        let line = timeline(
            secs(312),
            secs(1440),
            Some(&counting(312)),
            Some(&unmatched),
        )
        .unwrap();
        assert_eq!(line.middle.as_deref(), Some("records at 12:00 once linked"));
        assert!(!line.recording);
        let guess = Standing::NotRecording {
            why: Why::Unconfirmed,
            refused: false,
        };
        let line = timeline(secs(312), secs(1440), Some(&counting(312)), Some(&guess)).unwrap();
        assert_eq!(
            line.middle.as_deref(),
            Some("records at 12:00 once confirmed")
        );
    }

    #[test]
    fn a_settled_viewing_marks_nothing() {
        let refused = Standing::NotRecording {
            why: Why::Declined {
                decline: ryuuji_core::Decline::NotNext,
                next: Some(8),
            },
            refused: true,
        };
        let line = timeline(secs(842), secs(1440), Some(&counting(842)), Some(&refused)).unwrap();
        assert_eq!(
            (line.mark, line.middle, line.recording),
            (None, None, false)
        );
        let (_, watch) = recorded();
        let recorded = Standing::Recorded {
            watch,
            episode: Some(8..=8),
        };
        let line = timeline(secs(760), secs(1440), Some(&counting(760)), Some(&recorded)).unwrap();
        assert_eq!(
            (line.mark, line.middle.as_deref(), line.recording),
            (None, Some("recorded"), true)
        );
        assert_eq!(timeline(secs(60), Duration::ZERO, None, None), None);
    }

    #[test]
    fn the_middle_column_is_centred_on_the_mark() {
        for mark in [0.5, 0.2, 0.8, 0.0, 1.0] {
            let [left, middle, right] = mark_columns(mark).map(|length| match length {
                GridLength::Star(weight) => weight,
                GridLength::Pixel(_) | GridLength::Auto => 0.0,
            });
            assert!((left + middle / 2.0 - mark).abs() < 1e-9, "{mark}");
            assert!((left + middle + right - 1.0).abs() < 1e-9, "{mark}");
        }
    }

    #[test]
    fn the_hero_says_where_the_show_stands() {
        let (frieren, vinland) = two_shows();
        let status = show_info::status_line;
        assert_eq!(status(&frieren), "Watching \u{b7} 7 of 28 episodes");
        assert_eq!(status(&vinland), "Watching \u{b7} 2 episodes");
        let planned = LibraryEntry {
            status: WatchStatus::PlanToWatch,
            progress: 0,
            ..vinland.clone()
        };
        assert_eq!(status(&planned), "Plan to watch");
        let rewatch = LibraryEntry {
            status: WatchStatus::Completed,
            rewatching: true,
            progress: 3,
            ..frieren.clone()
        };
        assert_eq!(status(&rewatch), "Rewatching \u{b7} 3 of 28 episodes");
    }

    #[test]
    fn share_names_the_show_and_the_episode() {
        assert_eq!(
            share_text("Frieren: Beyond Journey's End", Some("Episode 8")),
            "Watching Frieren: Beyond Journey's End, episode 8"
        );
        assert_eq!(
            share_text("Frieren", Some("Episodes 1\u{2013}12")),
            "Watching Frieren, episodes 1\u{2013}12"
        );
        assert_eq!(share_text("Frieren", None), "Watching Frieren");
    }

    #[test]
    fn the_next_episode_is_offered_until_the_show_ends() {
        let (frieren, vinland) = two_shows();
        let file = |episode: u32| ProposedMatch {
            episode: Some(episode..=episode),
            ..proposal()
        };
        assert_eq!(next_after(&file(8), Some(&frieren)), Some(9));
        assert_eq!(next_after(&file(28), Some(&frieren)), None);
        assert_eq!(next_after(&file(40), Some(&vinland)), Some(41));
        assert_eq!(next_after(&file(8), None), None);
        assert_eq!(next_after(&proposal(), Some(&frieren)), None);
    }

    #[test]
    fn continue_watching_offers_the_shows_under_way_newest_first() {
        let (frieren, vinland) = two_shows();
        let now = SystemTime::UNIX_EPOCH + secs(3 * 86_400);
        let watched = |entry: &LibraryEntry, player: &str, at: u64| LastWatched {
            entry: entry.id,
            player: player.to_owned(),
            at: SystemTime::UNIX_EPOCH + secs(at),
        };
        let recent = [
            watched(&vinland, "vlc", 2 * 86_400),
            watched(&frieren, "mpv", 86_400),
        ];
        let rows = next_up(&[frieren.clone(), vinland.clone()], &recent, now);
        assert_eq!(
            rows,
            vec![
                NextUp {
                    entry: vinland.id,
                    title: "Vinland Saga Season 2".to_owned(),
                    line: "Episode 3 is next \u{b7} 2 \u{b7} last watched 1 d ago in vlc"
                        .to_owned(),
                    next: 3,
                },
                NextUp {
                    entry: frieren.id,
                    title: "Frieren: Beyond Journey's End".to_owned(),
                    line: "Episode 8 is next \u{b7} 7 / 28 \u{b7} last watched 2 d ago in mpv"
                        .to_owned(),
                    next: 8,
                },
            ]
        );
        // A finished or shelved show has nothing to pick up.
        let finished = LibraryEntry {
            progress: 28,
            ..frieren.clone()
        };
        let held = LibraryEntry {
            status: WatchStatus::OnHold,
            ..vinland.clone()
        };
        assert!(next_up(&[finished, held], &recent, now).is_empty());
    }

    #[test]
    fn the_picker_lists_the_guess_first_then_the_rest_by_title() {
        let (frieren, vinland) = two_shows();
        let shows = choices(&[vinland.clone(), frieren.clone()]);
        assert_eq!(
            shows
                .iter()
                .map(|show| (show.title.as_str(), show.line.as_str()))
                .collect::<Vec<_>>(),
            vec![
                ("Frieren: Beyond Journey's End", "Watching \u{b7} 7 / 28"),
                ("Vinland Saga Season 2", "Watching \u{b7} 2"),
            ]
        );
        let listed = offers(&shows, Some(vinland.id), "");
        assert_eq!(
            listed
                .iter()
                .map(|offer| (offer.show.entry, offer.likely))
                .collect::<Vec<_>>(),
            vec![(vinland.id, true), (frieren.id, false)]
        );
        assert_eq!(
            row_label(&shows[0]),
            "Frieren: Beyond Journey's End, Watching \u{b7} 7 / 28"
        );
    }

    #[test]
    fn the_picker_narrows_on_a_folded_title_and_keeps_everything_when_empty() {
        let (frieren, vinland) = two_shows();
        let shows = choices(&[frieren, vinland]);
        let titles = |query| {
            offers(&shows, None, query)
                .iter()
                .map(|offer| offer.show.title.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(titles("").len(), 2);
        // Punctuation folds away, so the colon need not be typed.
        assert_eq!(
            titles("frieren beyond journey"),
            ["Frieren: Beyond Journey's End"]
        );
        // A season phrase folds to its bare number, both sides alike.
        assert_eq!(titles("vinland saga 2"), ["Vinland Saga Season 2"]);
        assert!(titles("bleach").is_empty());
    }

    #[test]
    fn the_card_asks_by_itself_unless_the_match_is_settled() {
        let (frieren, _) = two_shows();
        let state = |link, ignored| AppState {
            library: vec![frieren.clone()],
            last_match: Some(ProposedMatch { link, ..proposal() }),
            ignored,
            ..AppState::default()
        };
        let asked = |link, ignored| {
            let question = question(&state(link, ignored)).unwrap();
            (
                question.open,
                question.likely,
                question.can_add,
                question.ignored,
            )
        };
        assert_eq!(asked(Link::Unmatched, false), (true, None, true, false));
        assert_eq!(
            asked(Link::Likely(frieren.id), false),
            (true, Some(frieren.id), false, false)
        );
        assert_eq!(
            asked(Link::Exact(frieren.id), false),
            (false, None, false, false)
        );
        // An ignored file is answered before its link, whatever the link.
        assert!(asked(Link::Exact(frieren.id), true).0);
        assert!(asked(Link::Unmatched, true).3);
        assert_eq!(question(&AppState::default()), None);
    }

    #[test]
    fn idle_placeholder_copy_branches_on_detection_down() {
        assert_eq!(
            idle_placeholder_copy(false),
            (
                "Nothing is playing",
                "Open an episode in your player and it will show up here."
            )
        );
        assert_eq!(idle_placeholder_copy(true).0, "Detection isn't running");
    }

    #[test]
    fn a_guessed_episode_says_what_else_it_could_be() {
        let read = |raw: &str| parse(raw, &Options::default());
        assert_eq!(
            episode_doubt(&read("Magical Girl Series 07.mkv")).as_deref(),
            Some("A guess: 07 may be a word of the title")
        );
        assert_eq!(episode_doubt(&read("[Subs] Show - 05 [1080p].mkv")), None);
    }

    /// A recording with an id of its own, which only a store mints: episode
    /// 1 of a show at 0, which leaves the show at 1.
    fn recorded() -> (LibraryEntry, ryuuji_core::HistoryId) {
        let tmp = tempfile::tempdir().unwrap();
        let dir = DataDir::at(tmp.path()).unwrap();
        let Opened { mut store, .. } = Store::open(&dir).unwrap();
        let entry = store
            .add(NewEntry {
                title: "Show".to_owned(),
                status: WatchStatus::Watching,
                progress: 0,
                total: None,
                rewatching: false,
            })
            .unwrap();
        let recording = store
            .record(NewRecording {
                watch: None,
                entry: entry.id,
                episode: 1..=1,
                raw_title: "Show - 01.mkv".to_owned(),
                parsed_title: "Show".to_owned(),
                player: "mpv".to_owned(),
            })
            .unwrap();
        (recording.entry, recording.watch.id)
    }

    #[test]
    fn undo_is_offered_while_the_show_stands_at_the_recording() {
        let (entry, watch) = recorded();
        let state = |progress, outcome| AppState {
            library: vec![LibraryEntry {
                progress,
                ..entry.clone()
            }],
            last_match: Some(ProposedMatch {
                link: Link::Exact(entry.id),
                episode: Some(1..=1),
                ..proposal()
            }),
            watch_progress: Some(WatchProgress {
                accrued: secs(720),
                threshold: secs(710),
                outcome,
            }),
            ..AppState::default()
        };
        assert!(undo_applies(&state(1, RecordOutcome::Recorded(watch))));
        assert!(!undo_applies(&state(2, RecordOutcome::Recorded(watch))));
        assert!(!undo_applies(&state(0, RecordOutcome::Undone)));
        assert!(!undo_applies(&AppState::default()));
    }

    #[test]
    fn fact_rows_say_what_the_parser_read() {
        let rows = fact_rows(&parse("[Subs] Show - 03 (1080p).mkv", &Options::default()));
        assert!(rows.contains(&("Release group".to_owned(), "Subs".to_owned())));
        assert!(rows.contains(&("Anime title".to_owned(), "Show".to_owned())));
        assert!(rows.contains(&("Episode number".to_owned(), "03".to_owned())));
    }

    #[test]
    fn fact_label_humanises_snake_case() {
        assert_eq!(fact_label("release_group"), "Release group");
        assert_eq!(fact_label("source"), "Source");
    }
}
