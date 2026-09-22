//! Page bodies. The Library lists real entries, Settings is laid out in the
//! Windows Settings idiom (section headers and cards) with the theme picker,
//! the data folder and the way into Diagnostics, and Now playing shows the
//! last playback observation on a card, or a placeholder in the symbolic
//! empty-state style the UI-direction research settled on (heading required,
//! neutral tone). History lives in its own module and is the one page handed
//! the core handle, since it reads its rows on demand. Details are built by
//! the shell, so this module never sees the log buffer.

use std::cell::RefCell;
use std::ops::RangeInclusive;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, SystemTime};

use ryuuji_core::{
    AppState, Certainty, Command, DataDir, Detail, ElementKind, EntryId, LibraryEntry, Link,
    Notice, NowPlaying, Options, Page, ProposedMatch, RecordOutcome, Ryuuji, StoreError,
    ThemePreference, WatchProgress, WatchStatus, error_chain, normalize_title, parse,
};
use windows_reactor::*;

use crate::history::{self, HistoryProps};
use crate::ui::{
    self, APP_VERSION, BUILD_PROFILE, CONTENT_MAX_WIDTH, FOLDER_GLYPH, REPAIR_GLYPH, age_of,
    caption, card, card_frame, enum_picker, episode_text, placeholder, section, table,
};

const PAGE_PADDING: f64 = 24.0;

/// How many shows a library needs before the picker offers a search box.
/// Below this the whole list is on screen, so a field to narrow it would be
/// one more thing to read for nothing.
const SEARCH_FROM: usize = 12;

/// One library row the picker can offer, worked out by the caller so the
/// page still never reasons about the library itself.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ShowChoice {
    entry: EntryId,
    title: String,
    /// `7 / 28`, or `7` on a show whose length is unknown.
    progress: String,
}

/// Every show that could be picked, by title, so the order does not shift
/// under the person as their progress changes.
fn choices(library: &[LibraryEntry]) -> Vec<ShowChoice> {
    let mut choices: Vec<ShowChoice> = library
        .iter()
        .map(|entry| ShowChoice {
            entry: entry.id,
            title: entry.title.clone(),
            progress: match entry.total {
                Some(total) => format!("{} / {total}", entry.progress),
                None => entry.progress.to_string(),
            },
        })
        .collect();
    choices.sort_by(|a, b| a.title.cmp(&b.title));
    choices
}

/// One library row as the page draws it, worked out by the caller for the
/// same reason as [`ShowChoice`].
#[derive(Clone, Debug, PartialEq, Eq)]
struct LibraryRow {
    entry: EntryId,
    title: String,
    /// `Completed · 3/24 · rewatching`.
    detail: String,
    /// Whether a rewatch is under way, and `None` on a show that is not
    /// finished: picking a finished show back up is the only thing the
    /// switch is for, so it has nothing to say anywhere else.
    rewatch: Option<bool>,
}

/// Every library row, in the order the store keeps them, which is the order
/// the shows were added. Nothing about a rewatch is a reason to re-sort the
/// library under the person while they are reading it.
fn library_rows(library: &[LibraryEntry]) -> Vec<LibraryRow> {
    library
        .iter()
        .map(|entry| LibraryRow {
            entry: entry.id,
            title: entry.title.clone(),
            detail: status_line(entry),
            rewatch: (entry.status == WatchStatus::Completed).then_some(entry.rewatching),
        })
        .collect()
}

/// Builds one page's body. Which page is the shell's decision, not this
/// module's, so nothing here reads [`AppState::page`].
pub fn body(
    page: Page,
    state: &AppState,
    dispatch: Dispatch<Command>,
    dir: &DataDir,
    detection_down: bool,
    core: &Rc<RefCell<Ryuuji>>,
) -> Element {
    match page {
        Page::Library => component(
            library,
            LibraryProps {
                dispatch,
                rows: library_rows(&state.library),
            },
        ),
        Page::NowPlaying => component(
            now_playing,
            NowPlayingProps {
                dispatch,
                now_playing: state.now_playing.clone(),
                last_match: state.last_match.clone(),
                watch_progress: state.watch_progress,
                title: state
                    .last_match
                    .as_ref()
                    .map(|m| m.title_in(&state.library).to_owned()),
                undo_applies: undo_applies(state),
                ignored: state.ignored,
                rewatching: rewatch_under_way(state),
                shows: choices(&state.library),
                detection_down,
            },
        ),
        Page::History => component(
            history::history,
            HistoryProps {
                dispatch,
                core: core.clone(),
                library: state.library.clone(),
            },
        ),
        Page::Settings => component(
            settings,
            SettingsProps {
                dispatch,
                theme: state.settings.theme,
                root: dir.root().to_path_buf(),
            },
        ),
    }
}

/// Wraps a body in what every destination shows around it: the notice bar
/// above, and the page padding.
pub fn chrome(notices: &[Notice], dispatch: Dispatch<Command>, body: Element) -> Element {
    grid((
        notice(notices, dispatch),
        border(body)
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

/// Shows the oldest notice, or nothing at all when the queue is empty.
/// Keyed on the queue length and the notice shown, so a dismissal remounts
/// the bar: `IsOpen` is diffed, and WinUI has already closed it. The length
/// alone is not enough, because a reread file replaces its notice in place,
/// and a pop and a replacement handled in one render leave the length as
/// it was.
fn notice(notices: &[Notice], dispatch: Dispatch<Command>) -> Element {
    let head = notices.first();
    let (severity, title, message) = match head {
        Some(Notice::SaveFailed { detail }) => (
            InfoBarSeverity::Error,
            "Couldn't save your change".to_owned(),
            detail.clone(),
        ),
        Some(Notice::FileProblem { file, problems }) => (
            InfoBarSeverity::Warning,
            format!(
                "{} has {}",
                file.file_name(),
                if problems.len() == 1 {
                    "a problem"
                } else {
                    "problems"
                }
            ),
            problems.join("\n"),
        ),
        Some(Notice::LibraryReset { backup }) => (
            InfoBarSeverity::Warning,
            "Your library was reset".to_owned(),
            format!(
                "The previous file wasn't a readable database. It was kept at {}.",
                backup.display()
            ),
        ),
        None => return Element::Empty,
    };
    InfoBar::new(title)
        .message(message)
        .severity(severity)
        .is_open(true)
        .on_closed(move || dispatch.call(Command::DismissNotice))
        .with_key(format!("{} {head:?}", notices.len()))
        .grid_row(0)
        .into()
}

/// What the Library page is handed. `dispatch` is fresh every render and
/// compares by identity, so it comes first and the props compare stops
/// there, as [`NowPlayingProps`] does.
#[derive(Clone, Debug, PartialEq, Eq)]
struct LibraryProps {
    dispatch: Dispatch<Command>,
    rows: Vec<LibraryRow>,
}

fn library(props: &LibraryProps, _cx: &mut RenderCx) -> Element {
    if props.rows.is_empty() {
        return placeholder(
            "Your library is empty",
            "Shows you track will appear here. Add one from Now playing when an episode is detected.",
        );
    }
    let dispatch = props.dispatch.clone();
    list_view(props.rows.clone(), move |row: &LibraryRow, _| {
        let main = vstack((
            text_block(row.title.clone()).semibold(),
            text_block(row.detail.clone()).foreground(ThemeRef::SecondaryText),
        ))
        .spacing(2.0)
        .vertical_alignment(VerticalAlignment::Center)
        .grid_column(0);
        let action: Element = match row.rewatch {
            Some(rewatching) => {
                let dispatch = dispatch.clone();
                let id = row.entry;
                button(rewatch_label(rewatching))
                    .on_click(move || {
                        dispatch.call(Command::SetRewatching {
                            id,
                            rewatching: !rewatching,
                        });
                    })
                    .vertical_alignment(VerticalAlignment::Center)
                    .grid_column(1)
                    .into()
            }
            None => Element::Empty,
        };
        grid(vec![main.into(), action]).columns([GridLength::STAR, GridLength::Auto])
    })
    .with_key_selector(|row: &LibraryRow| row.entry.to_string())
    .into()
}

/// What the row offers a finished show: the way into a rewatch, or the way
/// back out of one.
fn rewatch_label(rewatching: bool) -> &'static str {
    if rewatching {
        "Stop rewatching"
    } else {
        "Rewatch"
    }
}

fn status_line(entry: &LibraryEntry) -> String {
    let mut line = match entry.total {
        Some(total) => format!("{} · {}/{}", entry.status.label(), entry.progress, total),
        None => format!("{} · {}", entry.status.label(), entry.progress),
    };
    // Said on the row rather than in place of the status: the show is still
    // completed, and the count underneath it is a rewatch rather than a
    // finished show that somehow slipped back.
    if entry.rewatching {
        line.push_str(" · rewatching");
    }
    line
}

/// What Now playing shows. A standing proposal is captioned with its age,
/// so while nothing is playing the page runs a clock to keep that current.
/// `dispatch` is fresh every render and compares by identity, so it comes
/// first and the props compare stops there.
#[derive(Clone, Debug, PartialEq, Eq)]
struct NowPlayingProps {
    dispatch: Dispatch<Command>,
    now_playing: NowPlaying,
    last_match: Option<ProposedMatch>,
    watch_progress: Option<WatchProgress>,
    /// The proposal's title as the library knows it, resolved by the caller
    /// so the page never carries the library.
    title: Option<String>,
    /// Whether the viewing's recording can still be undone, decided by the
    /// caller for the same reason.
    undo_applies: bool,
    /// Whether this file is one the person said not to track. An ignored
    /// file reads as unmatched, so without this the card would offer to add
    /// the very show it was told to leave alone.
    ignored: bool,
    /// Whether the show the proposal names is being watched again, resolved
    /// by the caller for the same reason as `title`.
    rewatching: bool,
    /// The shows the picker can offer, resolved by the caller for the same
    /// reason as `title`.
    shows: Vec<ShowChoice>,
    detection_down: bool,
}

fn now_playing(props: &NowPlayingProps, cx: &mut RenderCx) -> Element {
    let idle = matches!(props.now_playing, NowPlaying::Idle);
    ui::use_refresh(cx, idle && props.last_match.is_some());

    let now_playing = &props.now_playing;
    let now = SystemTime::now();
    let top: Element = match now_playing {
        NowPlaying::Idle => {
            let (heading, body) = idle_placeholder_copy(props.detection_down);
            placeholder(heading, body)
        }
        NowPlaying::Detecting => placeholder(
            "Detecting…",
            "A player is open; waiting for something recognisable.",
        ),
        NowPlaying::Playing {
            title,
            player,
            status,
            position,
            duration,
        } => {
            let mut lines: Vec<Element> = vec![
                text_block(title.clone())
                    .font_size(20.0)
                    .semibold()
                    .wrap()
                    .into(),
                caption(format!("{} in {player}", status.label())).into(),
                text_block(format!(
                    "{} / {}",
                    clock_text(*position),
                    duration_text(*duration)
                ))
                .into(),
            ];
            if let Some(progress) = &props.watch_progress {
                let episode = props.last_match.as_ref().and_then(|m| m.episode.as_ref());
                let caption = caption(progress_text(progress, episode));
                lines.push(match progress.outcome {
                    RecordOutcome::Recorded(id) if props.undo_applies => {
                        let dispatch = props.dispatch.clone();
                        hstack((
                            button("Undo")
                                .on_click(move || dispatch.call(Command::UndoRecording(id))),
                            caption.vertical_alignment(VerticalAlignment::Center),
                        ))
                        .spacing(12.0)
                        .into()
                    }
                    RecordOutcome::Recorded(_)
                    | RecordOutcome::Counting
                    | RecordOutcome::Undone
                    | RecordOutcome::Declined(_)
                    | RecordOutcome::Ignored => caption.into(),
                });
            }
            card_frame(vstack(lines).spacing(4.0)).into()
        }
    };
    match &props.last_match {
        Some(m) => vstack((
            top,
            component(
                proposal_card,
                ProposalProps {
                    dispatch: props.dispatch.clone(),
                    m: m.clone(),
                    title: props
                        .title
                        .clone()
                        .unwrap_or_else(|| m.shown_title().to_owned()),
                    idle,
                    ignored: props.ignored,
                    rewatching: props.rewatching,
                    shows: props.shows.clone(),
                    now,
                },
            ),
        ))
        .spacing(8.0)
        .into(),
        None => top,
    }
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
            "Nothing playing",
            "Open an episode in your player and it will show up here.",
        )
    }
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
        .library
        .iter()
        .any(|shown| shown.id == entry && shown.progress == *episode.end())
}

/// Whether the show the standing proposal names is being watched again,
/// decided by the caller so the page never carries the library.
fn rewatch_under_way(state: &AppState) -> bool {
    let Some(entry) = state.last_match.as_ref().and_then(|m| m.link.entry()) else {
        return false;
    };
    state
        .library
        .iter()
        .any(|shown| shown.id == entry && shown.rewatching)
}

/// What the card says in place of the match facts once a file is ignored.
/// The confidence label would read "No library entry", which is true and
/// beside the point: nothing is being matched because nothing is meant to.
const IGNORED_BODY: &str = "Ryuuji won't propose this file again, or any file whose title reads \
                            the same. It keeps playing; nothing else changes.";

/// What the proposal card is handed. `dispatch` is fresh every render and
/// compares by identity, so it comes first and the props compare stops
/// there, as [`NowPlayingProps`] does. The card is a component of its own
/// because the picker's state belongs with the card that draws it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ProposalProps {
    dispatch: Dispatch<Command>,
    m: ProposedMatch,
    title: String,
    idle: bool,
    ignored: bool,
    /// Whether the show this names is being watched again, resolved by the
    /// caller for the same reason as `title`.
    rewatching: bool,
    shows: Vec<ShowChoice>,
    now: SystemTime,
}

fn proposal_card(props: &ProposalProps, cx: &mut RenderCx) -> Element {
    let (picking, set_picking) = cx.use_state(false);
    let (query, set_query) = cx.use_state(String::new());
    let (chosen, set_chosen) = cx.use_state(None::<EntryId>);

    let m = &props.m;
    let title = text_block(props.title.clone())
        .font_size(20.0)
        .semibold()
        .wrap();
    let body = if props.ignored {
        IGNORED_BODY.to_owned()
    } else {
        match_caption(m, props.idle, props.now, props.rewatching)
    };
    let mut children: Vec<Element> = vec![title.into(), caption(body).into()];
    let rows = fact_rows(m);
    if !rows.is_empty() {
        children.push(facts_grid(&rows));
    }
    let buttons: Vec<Element> = offers(m, props.ignored)
        .into_iter()
        .map(|offer| match offer {
            // The only offer that opens something rather than deciding
            // something, so it moves local state instead of dispatching.
            Offer::PickAnother => {
                let set_picking = set_picking.clone();
                button("Pick another show")
                    .on_click(move || set_picking.call(true))
                    .into()
            }
            Offer::Confirm(label) => {
                dispatching(&props.dispatch, label, Command::ConfirmProposedMatch)
            }
            Offer::Add => dispatching(
                &props.dispatch,
                "Add to library".to_owned(),
                Command::AddProposedToLibrary,
            ),
            Offer::Ignore => dispatching(
                &props.dispatch,
                "Ignore this file".to_owned(),
                Command::IgnoreFile,
            ),
            Offer::StopIgnoring => dispatching(
                &props.dispatch,
                "Stop ignoring".to_owned(),
                Command::StopIgnoring,
            ),
        })
        .collect();
    if !buttons.is_empty() {
        children.push(
            hstack(buttons)
                .spacing(8.0)
                .horizontal_alignment(HorizontalAlignment::Left)
                .into(),
        );
    }
    if picking {
        let shown = matching_shows(&props.shows, &query);
        let selected = selected_index(&shown, chosen);
        let mut section: Vec<Element> = vec![caption("Pick another show").into()];
        if props.shows.len() >= SEARCH_FROM {
            section.push(
                auto_suggest_box(query.clone())
                    .placeholder_text("Search your library")
                    .on_text_changed(set_query.clone())
                    .into(),
            );
        }
        let picked = shown.clone();
        let set_row = set_chosen.clone();
        section.push(
            list_view(shown, |show: &ShowChoice, _| {
                text_block(row_label(show)).wrap()
            })
            .with_key_selector(|show: &ShowChoice| show.entry.to_string())
            .selection_mode(SelectionMode::Single)
            .selected_index(selected)
            .on_selection_changed(move |index: i32| {
                set_row.call(
                    usize::try_from(index)
                        .ok()
                        .and_then(|index| picked.get(index))
                        .map(|show| show.entry),
                );
            })
            .height(200.0)
            .into(),
        );
        section.push(caption("Only shows already in your library.").into());
        let close = {
            let set_picking = set_picking.clone();
            let set_query = set_query.clone();
            let set_chosen = set_chosen.clone();
            move || {
                set_picking.call(false);
                set_query.call(String::new());
                set_chosen.call(None);
            }
        };
        let use_it = {
            let dispatch = props.dispatch.clone();
            let close = close.clone();
            move || {
                if let Some(entry) = chosen {
                    dispatch.call(Command::PickShow(entry));
                }
                close();
            }
        };
        section.push(
            hstack((
                button("Use this show").on_click(use_it),
                button("Cancel").on_click(close),
            ))
            .spacing(8.0)
            .horizontal_alignment(HorizontalAlignment::Left)
            .into(),
        );
        children.push(vstack(section).spacing(8.0).into());
    }
    card_frame(vstack(children).spacing(4.0)).into()
}

/// A card button that hands one command to the core and nothing else.
fn dispatching(dispatch: &Dispatch<Command>, label: String, command: Command) -> Element {
    let dispatch = dispatch.clone();
    button(label)
        .on_click(move || dispatch.call(command.clone()))
        .into()
}

/// What a picker row announces. WinUI names a list row after its content,
/// and a row built of two text blocks falls back to its position, so a
/// screen reader would read "1" rather than the show it is about to correct
/// an episode onto. Naming it outright says the show and where it stands,
/// in the order the row reads.
fn row_label(show: &ShowChoice) -> String {
    format!("{}, {}", show.title, show.progress)
}

/// Where the chosen show sits among the rows on screen, or -1 when it is not
/// among them. The choice is held by id and looked up here, never held as a
/// position: filtering moves rows under an index, so a kept index would
/// quietly select a different show and correct the episode onto one the
/// person never picked.
fn selected_index(shown: &[ShowChoice], chosen: Option<EntryId>) -> i32 {
    shown
        .iter()
        .position(|show| Some(show.entry) == chosen)
        .and_then(|index| i32::try_from(index).ok())
        .unwrap_or(-1)
}

/// The shows a query narrows to, folded the way matching compares titles, so
/// `frieren beyond journeys end` finds `Frieren: Beyond Journey's End`. An
/// empty query folds to nothing and keeps every show.
fn matching_shows(shows: &[ShowChoice], query: &str) -> Vec<ShowChoice> {
    let needle = normalize_title(query);
    shows
        .iter()
        .filter(|show| normalize_title(&show.title).contains(&needle))
        .cloned()
        .collect()
}

/// One thing the proposal card can offer under the facts.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Offer {
    /// A guess records nothing until the person says it names the right
    /// show, so the card asks, naming the episode at stake.
    Confirm(String),
    /// Nothing in the library matches, so the show can be added from here.
    Add,
    /// Name the show yourself, whatever the matcher decided.
    PickAnother,
    /// Stop proposing this file at all.
    Ignore,
    /// The way back, and the only thing an ignored file offers.
    StopIgnoring,
}

/// The buttons the card shows, in the order they sit. An ignored file reads
/// as unmatched, so it is answered before the link is: it offers the way
/// back rather than offering to add the show it was told to leave alone.
/// Everything else can be named by hand, a settled show included -- two
/// shows whose titles fold alike match exactly and wrongly, and that is the
/// case with nothing else to offer.
fn offers(m: &ProposedMatch, ignored: bool) -> Vec<Offer> {
    if ignored {
        return vec![Offer::StopIgnoring];
    }
    match m.link {
        Link::Likely(_) => vec![
            Offer::Confirm(match m.episode.as_ref() {
                Some(episode) => format!("Yes, record {}", episode_text(episode).to_lowercase()),
                None => "Yes, this is the show".to_owned(),
            }),
            Offer::PickAnother,
            Offer::Ignore,
        ],
        Link::Unmatched => vec![Offer::Add, Offer::Ignore],
        Link::Exact(_) => vec![Offer::PickAnother],
    }
}

fn match_caption(m: &ProposedMatch, idle: bool, now: SystemTime, rewatching: bool) -> String {
    let mut parts = Vec::new();
    if let Some(episode) = &m.episode {
        parts.push(episode_text(episode));
        parts.extend(episode_doubt(m));
    }
    parts.push(m.link.confidence().label().to_owned());
    // After the confidence, which says which show this is, and before the
    // idle tail, which is about the observation rather than the show.
    if rewatching {
        parts.push("Rewatching".to_owned());
    }
    if idle {
        parts.push(format!("Last seen in {}", m.player));
        parts.push(age_of(m.at, now));
    }
    parts.join(" \u{b7} ")
}

/// `A guess: 07 may be a word of the title`, when the parser had to guess
/// the episode. The caption already names the episode, so this says only
/// what else its text could have been. Re-parsed from the raw title like
/// the facts grid, so nothing about the doubt is carried on the proposal.
fn episode_doubt(m: &ProposedMatch) -> Option<String> {
    let reading = parse(&m.raw_title, &Options::default());
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

/// The countdown to the write, then what came of it.
fn progress_text(progress: &WatchProgress, episode: Option<&RangeInclusive<u32>>) -> String {
    let named = episode.map(|episode| episode_text(episode).to_lowercase());
    match (progress.outcome, named) {
        (RecordOutcome::Counting, _) => {
            let remaining = progress.threshold.saturating_sub(progress.accrued);
            format!("Recording in {}", clock_text(remaining))
        }
        (RecordOutcome::Recorded(_), Some(episode)) => format!("Recorded {episode}"),
        (RecordOutcome::Recorded(_), None) => "Recorded".to_owned(),
        (RecordOutcome::Undone, Some(episode)) => format!("Undid {episode}"),
        (RecordOutcome::Undone, None) => "Undone".to_owned(),
        (RecordOutcome::Declined(why), _) => why.label().to_owned(),
        // No episode is named: nothing is being counted towards, so saying
        // which one would suggest it still might be.
        (RecordOutcome::Ignored, _) => "Ignored".to_owned(),
    }
}

/// Every element the parser finds in the raw player title. Re-parsed here
/// rather than carried on the proposal, so a reloaded one reads the same.
fn fact_rows(m: &ProposedMatch) -> Vec<(String, String)> {
    parse(&m.raw_title, &Options::default())
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

fn facts_grid(rows: &[(String, String)]) -> Element {
    table([GridLength::Auto, GridLength::STAR])
        .spacing(6.0, 16.0)
        .rows(
            rows.iter()
                .map(|(label, value)| [caption(label.clone()), text_block(value.clone()).wrap()]),
        )
        .into()
}

/// `m:ss`, or `h:mm:ss` from one hour.
fn clock_text(value: Duration) -> String {
    let total = value.as_secs();
    let (hours, minutes, seconds) = (total / 3600, total / 60 % 60, total % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

/// Like [`clock_text`], with an unknown length shown as `--:--`.
fn duration_text(value: Duration) -> String {
    if value.is_zero() {
        "--:--".to_string()
    } else {
        clock_text(value)
    }
}

/// What Settings shows. The outcome of the last folder action is the page's
/// own, so it lives in the page rather than in the shell.
#[derive(Clone, Debug, PartialEq, Eq)]
struct SettingsProps {
    dispatch: Dispatch<Command>,
    theme: ThemePreference,
    root: PathBuf,
}

fn settings(props: &SettingsProps, cx: &mut RenderCx) -> Element {
    let (folder_action, set_folder_action) = cx.use_state(None::<String>);

    let dispatch = props.dispatch.clone();
    let open_diagnostics = {
        let dispatch = dispatch.clone();
        move || dispatch.call(Command::OpenDetail(Detail::Diagnostics))
    };
    scroll_viewer(
        vstack((
            vstack((section("Appearance"), theme_card(props.theme, dispatch))).spacing(8.0),
            vstack((
                section("Your data"),
                card(
                    Some(FOLDER_GLYPH),
                    "Data folder",
                    props.root.display().to_string(),
                    hstack((
                        ui::open_folder_button(props.root.clone(), set_folder_action),
                        caption(folder_action.unwrap_or_default())
                            .vertical_alignment(VerticalAlignment::Center),
                    ))
                    .spacing(12.0)
                    .into(),
                ),
                card(
                    Some(REPAIR_GLYPH),
                    "Diagnostics",
                    "Paths, schema version and recent log events",
                    button("Open diagnostics").on_click(open_diagnostics).into(),
                ),
            ))
            .spacing(8.0),
            caption(format!("Ryuuji {APP_VERSION} · {BUILD_PROFILE} build")),
        ))
        .spacing(24.0)
        .max_width(CONTENT_MAX_WIDTH),
    )
    .into()
}

/// The theme picker, a drop-down like Windows Settings' "Choose your mode".
fn theme_card(theme: ThemePreference, dispatch: Dispatch<Command>) -> Border {
    let combo = enum_picker(
        &ThemePreference::ALL,
        ThemePreference::label,
        theme,
        move |chosen| dispatch.call(Command::SetTheme(chosen)),
    )
    .min_width(160.0);
    card(
        None,
        "Theme",
        "Follow Windows, or pick light or dark.",
        combo.into(),
    )
}

#[cfg(test)]
mod tests {
    use ryuuji_core::{Decline, Link, NewEntry, NewRecording, Opened, Store, WatchStatus};

    use super::*;

    #[test]
    fn clock_text_formats_minutes_and_hours() {
        assert_eq!(clock_text(Duration::ZERO), "0:00");
        assert_eq!(clock_text(Duration::from_secs(305)), "5:05");
        assert_eq!(clock_text(Duration::from_secs(3661)), "1:01:01");
    }

    #[test]
    fn duration_text_marks_zero_unknown() {
        assert_eq!(duration_text(Duration::ZERO), "--:--");
        assert_eq!(duration_text(Duration::from_secs(1420)), "23:40");
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
            at: std::time::SystemTime::UNIX_EPOCH,
        }
    }

    #[test]
    fn match_caption_joins_episode_confidence_and_idle_player() {
        let m = ProposedMatch {
            episode: Some(1..=1),
            ..proposal()
        };
        assert_eq!(
            match_caption(&m, true, SystemTime::UNIX_EPOCH, false),
            "Episode 1 \u{b7} No library entry \u{b7} Last seen in mpv \u{b7} 0 s ago"
        );
        let batch = ProposedMatch {
            episode: Some(1..=12),
            ..proposal()
        };
        assert_eq!(
            match_caption(&batch, false, SystemTime::UNIX_EPOCH, false),
            "Episodes 1\u{2013}12 \u{b7} No library entry"
        );
        assert_eq!(
            match_caption(&proposal(), false, SystemTime::UNIX_EPOCH, false),
            "No library entry"
        );
    }

    #[test]
    fn match_caption_says_when_the_episode_was_a_guess() {
        let m = ProposedMatch {
            raw_title: "Byousoku 5 Centimeter [1080p].mkv".to_owned(),
            episode: Some(5..=5),
            ..proposal()
        };
        assert_eq!(
            match_caption(&m, false, SystemTime::UNIX_EPOCH, false),
            "Episode 5 \u{b7} A guess: 5 may be a word of the title \u{b7} No library entry"
        );
        let padded = ProposedMatch {
            raw_title: "Magical Girl Series 07.mkv".to_owned(),
            episode: Some(7..=7),
            ..proposal()
        };
        assert_eq!(
            match_caption(&padded, false, SystemTime::UNIX_EPOCH, false),
            "Episode 7 \u{b7} A guess: 07 may be a word of the title \u{b7} No library entry"
        );
        let sure = ProposedMatch {
            raw_title: "[Subs] Show - 05 [1080p].mkv".to_owned(),
            episode: Some(5..=5),
            ..proposal()
        };
        assert_eq!(
            match_caption(&sure, false, SystemTime::UNIX_EPOCH, false),
            "Episode 5 \u{b7} No library entry"
        );
    }

    #[test]
    fn match_caption_appends_last_seen_and_age_when_idle() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(2 * 3600);
        assert_eq!(
            match_caption(&proposal(), true, now, false),
            "No library entry \u{b7} Last seen in mpv \u{b7} 2 h ago"
        );
        assert_eq!(
            match_caption(&proposal(), false, now, false),
            "No library entry"
        );
    }

    #[test]
    fn match_caption_says_when_the_show_is_being_watched_again() {
        let m = ProposedMatch {
            episode: Some(1..=1),
            ..proposal()
        };
        assert_eq!(
            match_caption(&m, false, SystemTime::UNIX_EPOCH, true),
            "Episode 1 \u{b7} No library entry \u{b7} Rewatching"
        );
        // After the confidence and before the idle tail, which is about the
        // observation rather than the show.
        assert_eq!(
            match_caption(&m, true, SystemTime::UNIX_EPOCH, true),
            "Episode 1 \u{b7} No library entry \u{b7} Rewatching \u{b7} Last seen in mpv \u{b7} 0 s ago"
        );
    }

    #[test]
    fn a_rewatch_is_read_off_the_show_the_proposal_names() {
        let (entry, _) = recorded_show();
        let state = |link, rewatching| AppState {
            library: vec![LibraryEntry {
                rewatching,
                ..entry.clone()
            }],
            last_match: Some(ProposedMatch { link, ..proposal() }),
            ..AppState::default()
        };
        assert!(rewatch_under_way(&state(Link::Exact(entry.id), true)));
        assert!(!rewatch_under_way(&state(Link::Exact(entry.id), false)));
        // A guess names a show as well, and the card says the same of it.
        assert!(rewatch_under_way(&state(Link::Likely(entry.id), true)));
        // No show is named, so there is nothing to be watching again.
        assert!(!rewatch_under_way(&state(Link::Unmatched, true)));
        assert!(!rewatch_under_way(&AppState::default()));
    }

    /// Only the store mints ids, so the Recorded arm needs a show and a
    /// recording made for real: episode 1, which leaves progress at 1.
    fn recorded_show() -> (LibraryEntry, ryuuji_core::HistoryId) {
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

    fn recorded_watch_id() -> ryuuji_core::HistoryId {
        recorded_show().1
    }

    #[test]
    fn the_card_asks_about_a_guess_offers_to_add_an_unmatched_show_and_can_ignore_either() {
        let (entry, _) = recorded_show();
        let guess = |episode| ProposedMatch {
            link: Link::Likely(entry.id),
            episode,
            ..proposal()
        };
        assert_eq!(
            offers(&guess(Some(1..=1)), false),
            vec![
                Offer::Confirm("Yes, record episode 1".to_owned()),
                Offer::PickAnother,
                Offer::Ignore
            ]
        );
        assert_eq!(
            offers(&guess(Some(1..=12)), false),
            vec![
                Offer::Confirm("Yes, record episodes 1\u{2013}12".to_owned()),
                Offer::PickAnother,
                Offer::Ignore
            ]
        );
        assert_eq!(
            offers(&guess(None), false),
            vec![
                Offer::Confirm("Yes, this is the show".to_owned()),
                Offer::PickAnother,
                Offer::Ignore
            ]
        );
        assert_eq!(offers(&proposal(), false), vec![Offer::Add, Offer::Ignore]);
        // A settled show used to offer nothing, which left the commonest
        // wrong match -- two titles that fold alike -- with no way out.
        let exact = ProposedMatch {
            link: Link::Exact(entry.id),
            ..proposal()
        };
        assert_eq!(offers(&exact, false), vec![Offer::PickAnother]);
    }

    /// The seeded show shelved somewhere else; only the status, the numbers
    /// and the flag matter here, and the shell cannot mint an entry id.
    fn shelved(
        entry: &LibraryEntry,
        status: WatchStatus,
        progress: u32,
        total: Option<u32>,
        rewatching: bool,
    ) -> LibraryEntry {
        LibraryEntry {
            status,
            progress,
            total,
            rewatching,
            ..entry.clone()
        }
    }

    #[test]
    fn only_a_finished_show_is_offered_a_rewatch() {
        let (entry, _) = recorded_show();
        let rows = library_rows(&[
            shelved(&entry, WatchStatus::Watching, 7, Some(28), false),
            shelved(&entry, WatchStatus::Completed, 24, Some(24), false),
            shelved(&entry, WatchStatus::Completed, 3, Some(24), true),
            shelved(&entry, WatchStatus::Completed, 5, None, true),
        ]);
        // Picking a finished show back up is the only thing the switch is
        // for, so a show still being watched is offered nothing.
        assert_eq!(
            rows.iter().map(|row| row.rewatch).collect::<Vec<_>>(),
            vec![None, Some(false), Some(true), Some(true)]
        );
        assert_eq!(rows[0].detail, "Watching \u{b7} 7/28");
        assert_eq!(rows[1].detail, "Completed \u{b7} 24/24");
        assert_eq!(rows[2].detail, "Completed \u{b7} 3/24 \u{b7} rewatching");
        assert_eq!(rows[3].detail, "Completed \u{b7} 5 \u{b7} rewatching");
    }

    #[test]
    fn the_rewatch_button_says_which_way_it_goes() {
        assert_eq!(rewatch_label(false), "Rewatch");
        assert_eq!(rewatch_label(true), "Stop rewatching");
    }

    /// A library row with the same id as the seeded show; only the title
    /// and the numbers matter here, and the shell cannot mint an entry id.
    fn choice(
        entry: &LibraryEntry,
        title: &str,
        progress: u32,
        total: Option<u32>,
    ) -> LibraryEntry {
        LibraryEntry {
            title: title.to_owned(),
            progress,
            total,
            ..entry.clone()
        }
    }

    #[test]
    fn the_picker_lists_shows_by_title_with_their_progress() {
        let (entry, _) = recorded_show();
        let shows = choices(&[
            choice(&entry, "Vinland Saga", 2, Some(24)),
            choice(&entry, "Frieren", 7, None),
        ]);
        assert_eq!(
            shows
                .iter()
                .map(|show| show.title.as_str())
                .collect::<Vec<_>>(),
            vec!["Frieren", "Vinland Saga"]
        );
        assert_eq!(shows[0].progress, "7");
        assert_eq!(shows[1].progress, "2 / 24");
    }

    /// Two library rows with ids of their own. The shell cannot mint an
    /// entry id, so they come from a real store.
    fn two_shows() -> (LibraryEntry, LibraryEntry) {
        let tmp = tempfile::tempdir().unwrap();
        let dir = DataDir::at(tmp.path()).unwrap();
        let Opened { mut store, .. } = Store::open(&dir).unwrap();
        let first = store
            .add(NewEntry {
                title: "Frieren".to_owned(),
                status: WatchStatus::Watching,
                progress: 7,
                total: Some(28),
                rewatching: false,
            })
            .unwrap();
        let second = store
            .add(NewEntry {
                title: "Vinland Saga".to_owned(),
                status: WatchStatus::Watching,
                progress: 2,
                total: Some(24),
                rewatching: false,
            })
            .unwrap();
        (first, second)
    }

    #[test]
    fn a_picker_row_announces_the_show_rather_than_its_position() {
        let (entry, _) = recorded_show();
        let shows = choices(&[
            choice(&entry, "Frieren", 7, Some(28)),
            choice(&entry, "Vinland Saga", 2, None),
        ]);
        assert_eq!(row_label(&shows[0]), "Frieren, 7 / 28");
        assert_eq!(row_label(&shows[1]), "Vinland Saga, 2");
    }

    #[test]
    fn the_picker_follows_the_chosen_show_rather_than_its_position() {
        let (frieren, vinland) = two_shows();
        let shows = choices(&[frieren.clone(), vinland.clone()]);
        assert_eq!(selected_index(&shows, None), -1);
        assert_eq!(selected_index(&shows, Some(frieren.id)), 0);
        assert_eq!(selected_index(&shows, Some(vinland.id)), 1);

        // Narrowing drops the first row, so the chosen show moves from the
        // second position to the first. Held as a position it would still
        // read 1, which is now either another show or nothing at all.
        let narrowed = matching_shows(&shows, "vinland");
        assert_eq!(narrowed.len(), 1);
        assert_eq!(selected_index(&narrowed, Some(vinland.id)), 0);
        assert_eq!(selected_index(&narrowed, Some(frieren.id)), -1);
    }

    #[test]
    fn the_picker_narrows_on_a_folded_title_and_keeps_everything_when_empty() {
        let (entry, _) = recorded_show();
        let shows = choices(&[
            choice(&entry, "Frieren: Beyond Journey's End", 7, Some(28)),
            choice(&entry, "Vinland Saga Season 2", 2, Some(24)),
        ]);
        let titles = |query| {
            matching_shows(&shows, query)
                .iter()
                .map(|show| show.title.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(matching_shows(&shows, "").len(), 2);
        assert_eq!(titles("frieren beyond"), ["Frieren: Beyond Journey's End"]);
        // Punctuation folds away, so the colon need not be typed.
        assert_eq!(
            titles("frieren beyond journey"),
            ["Frieren: Beyond Journey's End"]
        );
        // An apostrophe folds to a space rather than closing the word up, so
        // `Journey's` reads as two words: typing the contraction whole finds
        // nothing. The same fold decides every match in the app, so the
        // picker agrees with the matcher rather than being kinder than it.
        assert!(titles("journeys").is_empty());
        assert_eq!(titles("journey"), ["Frieren: Beyond Journey's End"]);
        // A season phrase folds to its bare number, both sides alike.
        assert_eq!(titles("vinland saga 2"), ["Vinland Saga Season 2"]);
        assert!(matching_shows(&shows, "bleach").is_empty());
    }

    #[test]
    fn an_ignored_file_offers_only_the_way_back() {
        let (entry, _) = recorded_show();
        // Whatever the link says, because an ignored file is answered before
        // the link is: it reads as unmatched, which would otherwise offer to
        // add the show it was told to leave alone.
        for link in [
            Link::Unmatched,
            Link::Likely(entry.id),
            Link::Exact(entry.id),
        ] {
            let m = ProposedMatch { link, ..proposal() };
            assert_eq!(offers(&m, true), vec![Offer::StopIgnoring], "{link:?}");
        }
    }

    #[test]
    fn undo_is_offered_while_the_show_stands_at_the_recording() {
        let (entry, watch) = recorded_show();
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
                accrued: Duration::from_secs(720),
                threshold: Duration::from_secs(710),
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
    fn progress_text_counts_down_then_says_what_came_of_it() {
        let counting = WatchProgress {
            accrued: Duration::from_secs(510),
            threshold: Duration::from_secs(710),
            outcome: RecordOutcome::Counting,
        };
        assert_eq!(
            progress_text(&counting, Some(&(3..=3))),
            "Recording in 3:20"
        );
        assert_eq!(progress_text(&counting, None), "Recording in 3:20");
        let overshot = WatchProgress {
            accrued: Duration::from_secs(800),
            ..counting
        };
        assert_eq!(progress_text(&overshot, None), "Recording in 0:00");

        let recorded = WatchProgress {
            outcome: RecordOutcome::Recorded(recorded_watch_id()),
            ..counting
        };
        assert_eq!(
            progress_text(&recorded, Some(&(3..=3))),
            "Recorded episode 3"
        );
        assert_eq!(
            progress_text(&recorded, Some(&(1..=12))),
            "Recorded episodes 1\u{2013}12"
        );
        assert_eq!(progress_text(&recorded, None), "Recorded");

        let undone = WatchProgress {
            outcome: RecordOutcome::Undone,
            ..counting
        };
        // An ignored file names no episode: nothing is being counted
        // towards, so naming one would suggest it still might be.
        let ignored = WatchProgress {
            outcome: RecordOutcome::Ignored,
            ..counting
        };
        assert_eq!(progress_text(&ignored, Some(&(3..=3))), "Ignored");
        assert_eq!(progress_text(&ignored, None), "Ignored");

        assert_eq!(progress_text(&undone, Some(&(3..=3))), "Undid episode 3");
        assert_eq!(
            progress_text(&undone, Some(&(1..=12))),
            "Undid episodes 1\u{2013}12"
        );
        assert_eq!(progress_text(&undone, None), "Undone");

        for (why, text) in [
            (Decline::NotExact, "Not recorded: no exact match"),
            (Decline::Completed, "Not recorded: show is completed"),
            (Decline::NoEpisode, "Not recorded: no episode number"),
            (Decline::NotNext, "Not recorded: not the next episode"),
            (Decline::PastTotal, "Not recorded: past the show's total"),
        ] {
            let declined = WatchProgress {
                outcome: RecordOutcome::Declined(why),
                ..counting
            };
            assert_eq!(progress_text(&declined, Some(&(3..=3))), text);
            assert_eq!(progress_text(&declined, None), text);
        }
    }

    #[test]
    fn idle_placeholder_copy_branches_on_detection_down() {
        assert_eq!(
            idle_placeholder_copy(false),
            (
                "Nothing playing",
                "Open an episode in your player and it will show up here."
            )
        );
        assert_eq!(
            idle_placeholder_copy(true),
            (
                "Detection isn't running",
                "Ryuuji can't watch your players right now. See Diagnostics in Settings for what went wrong."
            )
        );
    }

    #[test]
    fn fact_rows_re_parse_the_raw_title() {
        let m = ProposedMatch {
            raw_title: "[Subs] Show - 03 (1080p).mkv".to_owned(),
            ..proposal()
        };
        let rows = fact_rows(&m);
        assert!(rows.contains(&("Release group".to_owned(), "Subs".to_owned())));
        assert!(rows.contains(&("Anime title".to_owned(), "Show".to_owned())));
        assert!(rows.contains(&("Episode number".to_owned(), "03".to_owned())));
    }

    #[test]
    fn fact_label_humanises_snake_case() {
        assert_eq!(fact_label("release_group"), "Release group");
        assert_eq!(fact_label("anime_title"), "Anime title");
        assert_eq!(fact_label("source"), "Source");
    }
}
