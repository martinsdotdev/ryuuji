//! The Diagnostics page: where the files are, what state they are in, and
//! the tail of the log, with the whole thing copyable as plain text.

use std::cell::RefCell;
use std::fmt::Write as _;
use std::ops::RangeInclusive;
use std::path::Path;
use std::rc::Rc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ryuuji_core::{
    Account, AppState, Command, DataDirSource, Diagnostics, FileFacts, FileStat, Link, Listing,
    PlaybackEvent, PlaybackSource, PlaybackStatus, ProbeFailed, ProposedMatch, Recorded, Ryuuji,
    SchemaVersion, Service, Signed, Task, Tie, TimeZone, Watch, WatchOutcome, Work, date_time,
    error_chain,
};
use ryuuji_detect::{SessionFacts, WatchError, Watcher};
use ryuuji_sync::{LastCall, Pause, Paused, ServiceFacts, SyncError, Syncer};
use tracing::{Level, warn};
use windows_reactor::*;

use crate::logging::{EventRecord, RecentEvents};
use crate::ui::{
    self, APP_VERSION, BUILD_PROFILE, CONTENT_MAX_WIDTH, FOLDER_GLYPH, MONO_FONT, age_of, caption,
    card, card_frame, enum_picker, open_folder_button, section, table,
};

const NOT_APPLICABLE: &str = "—";

pub const INJECTED_TITLE: &str =
    "[SubsPlease] Frieren - Beyond Journey's End - 01 (1080p) [ABCD1234].mkv";
pub const INJECTED_PLAYER: &str = "Injected";

/// The canned event the Diagnostics page feeds through `Command::Playback`.
pub fn injected(status: PlaybackStatus) -> PlaybackEvent {
    PlaybackEvent {
        player: INJECTED_PLAYER.to_owned(),
        title: INJECTED_TITLE.to_owned(),
        status,
        position: Duration::from_secs(5 * 60),
        duration: Duration::from_secs(24 * 60),
        observed_at: SystemTime::now(),
        source: PlaybackSource::Injected,
        foreground: None,
    }
}

/// Which events the page shows. The report always carries all of them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventLevelFilter {
    All,
    WarnAndAbove,
    Error,
}

impl EventLevelFilter {
    pub const ALL: [EventLevelFilter; 3] = [
        EventLevelFilter::All,
        EventLevelFilter::WarnAndAbove,
        EventLevelFilter::Error,
    ];

    pub fn label(self) -> &'static str {
        match self {
            EventLevelFilter::All => "All",
            EventLevelFilter::WarnAndAbove => "Warnings and errors",
            EventLevelFilter::Error => "Errors",
        }
    }

    pub fn admits(self, level: Level) -> bool {
        match self {
            EventLevelFilter::All => true,
            EventLevelFilter::WarnAndAbove => level == Level::WARN || level == Level::ERROR,
            EventLevelFilter::Error => level == Level::ERROR,
        }
    }
}

/// Everything the page shows, gathered at one moment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Report {
    /// When the report was gathered; every age it prints is relative to this.
    pub now: SystemTime,
    pub core: Diagnostics,
    /// The core's latest proposal, as shown on Now playing.
    pub last_match: Option<ProposedMatch>,
    pub events: Vec<EventRecord>,
    /// Every SMTC session at the last refresh, or why detection is off.
    pub sessions: std::result::Result<Vec<SessionFacts>, String>,
    /// Each list service: its lane, its account and its shows.
    pub lists: Vec<ListFacts>,
    /// Why the sync worker is not running, while it is not.
    pub sync_down: Option<String>,
    /// The zone the watches' times are printed in.
    pub zone: TimeZone,
}

impl Report {
    /// Reads every source at one instant. Called from the page's own
    /// render, so its refresh clock re-reads what it shows.
    pub fn gather(
        core: &Ryuuji,
        recent: &RecentEvents,
        watcher: &std::result::Result<Watcher, WatchError>,
        syncer: &std::result::Result<Syncer, SyncError>,
    ) -> Report {
        let sessions = watcher
            .as_ref()
            .map(Watcher::sessions)
            .map_err(|err| error_chain(err));
        let lanes = syncer.as_ref().map(Syncer::facts).unwrap_or_default();
        Report::new(
            core.diagnostics(),
            core.state().last_match.clone(),
            recent.snapshot(),
            sessions,
            list_facts(core.state(), &lanes),
            syncer.as_ref().err().map(|err| error_chain(err)),
            ui::local_zone(),
        )
    }

    pub fn new(
        core: Diagnostics,
        last_match: Option<ProposedMatch>,
        events: Vec<EventRecord>,
        sessions: std::result::Result<Vec<SessionFacts>, String>,
        lists: Vec<ListFacts>,
        sync_down: Option<String>,
        zone: TimeZone,
    ) -> Report {
        Report {
            now: SystemTime::now(),
            core,
            last_match,
            events,
            sessions,
            lists,
            sync_down,
            zone,
        }
    }

    /// The plain-text form that goes to the clipboard, oldest event first
    /// like the log it excerpts. Every event is included whatever the page's
    /// level filter shows.
    pub fn to_text(&self) -> String {
        let now = self.now;
        let core = &self.core;
        let mut out = String::new();
        let _ = writeln!(out, "Ryuuji {APP_VERSION} ({BUILD_PROFILE})");
        let _ = writeln!(
            out,
            "Data directory: {} [{}]",
            core.data_dir.display(),
            source_label(core.data_dir_source)
        );
        let _ = writeln!(out, "Library: {}", file_line(&core.library, now));
        let _ = writeln!(out, "Schema version: {}", schema_text(&core.schema));
        let _ = writeln!(out, "Settings: {}", file_line(&core.settings, now));
        let _ = writeln!(out, "Players: {}", file_line(&core.players, now));
        let _ = writeln!(out, "Logs: {}", core.logs.display());
        let current_log = core
            .current_log
            .as_ref()
            .map_or_else(|| "none".to_owned(), |facts| file_line(facts, now));
        let _ = writeln!(out, "Current log: {current_log}");
        match &self.last_match {
            Some(last) => {
                let _ = writeln!(out, "Last match:");
                let _ = writeln!(out, "  raw: {}", last.raw_title);
                let title = if last.parsed_title.is_empty() {
                    NOT_APPLICABLE
                } else {
                    last.parsed_title.as_str()
                };
                let _ = writeln!(out, "  title: {title}");
                let _ = writeln!(
                    out,
                    "  episode: {}  season: {}  group: {}",
                    episode_text(last.episode.as_ref()),
                    count_text(last.season),
                    last.release_group.as_deref().unwrap_or(NOT_APPLICABLE)
                );
                let _ = writeln!(
                    out,
                    "  entry: {}  confidence: {}",
                    last.link
                        .entry()
                        .map_or_else(|| NOT_APPLICABLE.to_owned(), |id| id.to_string()),
                    last.link.confidence().tag()
                );
                let _ = writeln!(
                    out,
                    "  player: {}  at: {}",
                    last.player,
                    last.at
                        .duration_since(UNIX_EPOCH)
                        .map_or(0, |elapsed| elapsed.as_secs())
                );
            }
            None => {
                let _ = writeln!(out, "Last match: none");
            }
        }
        let _ = writeln!(out);
        match &core.watches {
            Ok(watches) => {
                let _ = writeln!(out, "Watches ({}):", watches.len());
                for watch in watches {
                    let _ = writeln!(out, "  {}", watch_line(watch, &self.zone));
                }
            }
            Err(ProbeFailed { detail }) => {
                let _ = writeln!(out, "Watches: unreadable ({detail})");
            }
        }
        let _ = writeln!(out);
        let _ = writeln!(out, "Media sessions:");
        match &self.sessions {
            Ok(sessions) if sessions.is_empty() => {
                let _ = writeln!(out, "  none");
            }
            Ok(sessions) => {
                for session in sessions {
                    let _ = writeln!(out, "  {}", session_line(session));
                }
            }
            Err(err) => {
                let _ = writeln!(out, "  unavailable: {err}");
            }
        }
        let _ = writeln!(out);
        let _ = writeln!(out, "Lists:");
        if let Some(why) = &self.sync_down {
            let _ = writeln!(out, "  sync not running: {why}");
        }
        for list in &self.lists {
            let _ = writeln!(out, "  {}", list.service.label());
            for (label, value) in list_rows(list, now, &self.zone) {
                let _ = writeln!(out, "    {label}: {value}");
            }
        }
        let _ = writeln!(out);
        let _ = writeln!(out, "Recent events ({}):", self.events.len());
        for event in &self.events {
            let _ = writeln!(out, "{}", event_line(event));
        }
        out
    }
}

/// One list service as the Lists section shows it: the worker's lane, the
/// account the core holds, and where its shows stand. None of it is a
/// token: the lane's facts never carry one, and the account is the stored
/// row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListFacts {
    pub service: Service,
    pub account: Option<Account>,
    /// The lane as the worker last published it; `None` while the worker
    /// is not running.
    pub lane: Option<ServiceFacts>,
    pub shows: ShowCounts,
}

/// How the shows tied to a service stand, counted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ShowCounts {
    pub linked: usize,
    /// The reconciles the core posts now: linked shows whose list differs
    /// from what the library wants, and the ones the watched-ahead rule
    /// asked about. Nothing while no account is signed in.
    pub waiting: usize,
    pub parked: usize,
    pub unpicked: usize,
    pub gone: usize,
    pub off: usize,
}

/// The Lists facts, one per service, from the core's state and the lanes
/// the worker published.
pub fn list_facts(state: &AppState, lanes: &[ServiceFacts]) -> Vec<ListFacts> {
    let work = state.work();
    Service::ALL
        .into_iter()
        .map(|service| ListFacts {
            service,
            account: state
                .sync
                .accounts
                .iter()
                .find(|account| account.service == service)
                .cloned(),
            lane: lanes.iter().find(|lane| lane.service == service).cloned(),
            shows: show_counts(&state.sync.listings, &work, service),
        })
        .collect()
}

fn show_counts(listings: &[Listing], work: &Work, service: Service) -> ShowCounts {
    let mut counts = ShowCounts::default();
    for listing in listings.iter().filter(|listing| listing.service == service) {
        match listing.tie {
            Tie::Linked { .. } => {
                counts.linked += 1;
                if listing.parked().is_some() {
                    counts.parked += 1;
                }
            }
            Tie::Unpicked { .. } => counts.unpicked += 1,
            Tie::Gone { .. } => counts.gone += 1,
            Tie::Off => counts.off += 1,
        }
    }
    counts.waiting = work
        .services
        .iter()
        .filter(|posted| posted.service == service)
        .flat_map(|posted| &posted.jobs)
        .filter(|job| matches!(job.task, Task::Reconcile { .. }))
        .count();
    counts
}

/// The facts one service gets, as label and value, in the order the report
/// and the card both show them. The lane's rows are left out while the
/// worker is not running.
fn list_rows(list: &ListFacts, now: SystemTime, zone: &TimeZone) -> Vec<(&'static str, String)> {
    let mut rows = vec![("Standing", standing_text(list, now, zone))];
    if let Some(lane) = &list.lane {
        rows.push(("Paused", pause_text(lane.paused, zone)));
        rows.push(("Rate remaining", count_text(lane.rate_remaining)));
        rows.push(("Last call", last_call_text(lane.last_call, zone)));
        rows.push((
            "Jobs",
            format!(
                "{} due, {} waiting, {} running",
                lane.due, lane.waiting, lane.running
            ),
        ));
    }
    rows.push(("Shows", shows_text(&list.shows)));
    rows
}

/// The account as the core holds it, with the one thing the worker alone
/// knows laid over a signed-in one: a refusal the core has not taken yet.
/// The expiry is judged against `now` here, since the core keeps the time
/// the service gave and not a verdict on it.
fn standing_text(list: &ListFacts, now: SystemTime, zone: &TimeZone) -> String {
    let Some(account) = &list.account else {
        return "not signed in".to_owned();
    };
    match &account.signed {
        Signed::Out => "signed out".to_owned(),
        Signed::Refused { message } => format!("refused: {message}"),
        Signed::In => {
            if let Some(message) = list.lane.as_ref().and_then(|lane| lane.stopped.as_deref()) {
                format!("refused: {message}")
            } else if account.expires.is_some_and(|at| at <= now) {
                "token expired".to_owned()
            } else {
                signed_in_text(account, zone)
            }
        }
    }
}

/// `signed in as someone (user 7), token works until 2026-09-23 00:00`,
/// with the expiry only where the service gave one.
fn signed_in_text(account: &Account, zone: &TimeZone) -> String {
    let mut text = format!("signed in as {} (user {})", account.name, account.user.0);
    if let Some(at) = account.expires {
        let _ = write!(text, ", token works until {}", date_time(at, zone));
    }
    text
}

fn pause_text(paused: Option<Paused>, zone: &TimeZone) -> String {
    match paused {
        Some(Paused { why, until }) => {
            let why = match why {
                Pause::Throttled => "throttled",
                Pause::Budget => "rate budget spent",
                Pause::Offline => "offline",
            };
            format!("{why} until {}", date_time(until, zone))
        }
        None => "no".to_owned(),
    }
}

fn last_call_text(last: Option<LastCall>, zone: &TimeZone) -> String {
    match last {
        Some(LastCall {
            at,
            status: Some(status),
        }) => format!("{} status {status}", date_time(at, zone)),
        Some(LastCall { at, status: None }) => format!("{} unreachable", date_time(at, zone)),
        None => "none".to_owned(),
    }
}

fn shows_text(shows: &ShowCounts) -> String {
    format!(
        "linked {}, waiting to send {}, parked {}, unpicked {}, gone {}, off {}",
        shows.linked, shows.waiting, shows.parked, shows.unpicked, shows.gone, shows.off
    )
}

/// A count, or the em dash when absent.
fn count_text(value: Option<u32>) -> String {
    value.map_or_else(|| NOT_APPLICABLE.to_owned(), |n| n.to_string())
}

/// An episode span, `3` alone or `1-12` for a batch, or the em dash when
/// absent. Plain hyphen: the report is plain text.
fn episode_text(value: Option<&RangeInclusive<u32>>) -> String {
    value.map_or_else(
        || NOT_APPLICABLE.to_owned(),
        |range| {
            if range.start() == range.end() {
                range.start().to_string()
            } else {
                format!("{}-{}", range.start(), range.end())
            }
        },
    )
}

/// `app_id | title | status | player`, matching the sessions card.
fn session_line(session: &SessionFacts) -> String {
    format!(
        "{} | {} | {} | {}",
        session.app_id,
        session.title,
        session.status,
        player_label(session)
    )
}

/// The player that claimed the session, marked when a person turned it off,
/// or [`NOT_APPLICABLE`] when nothing claimed it.
fn player_label(session: &SessionFacts) -> String {
    match &session.player {
        Some(name) if session.hidden => format!("{name} (hidden)"),
        Some(name) => name.clone(),
        None => NOT_APPLICABLE.to_owned(),
    }
}

/// `#id | opened | outcome | match | raw title | player`, matching the
/// watches card.
fn watch_line(watch: &Watch, zone: &TimeZone) -> String {
    format!(
        "#{} | {} | {} | {} | {} | {}",
        watch.id,
        date_time(watch.at, zone),
        outcome_text(watch),
        match_text(&watch.link),
        watch.raw_title,
        watch.player
    )
}

/// What the row says in the store's own words: `recorded 7→8`, a decline's
/// tag, or `added`, then `, added` or `, undone` for a mark on top.
fn outcome_text(watch: &Watch) -> String {
    let mut text = match watch.outcome {
        WatchOutcome::Recorded(Recorded {
            progress_before,
            progress,
            ..
        }) => format!("recorded {progress_before}\u{2192}{progress}"),
        WatchOutcome::Declined(decline) => decline.tag().to_owned(),
        WatchOutcome::Added => "added".to_owned(),
        WatchOutcome::Ignored => "ignored".to_owned(),
    };
    if watch.added_at.is_some() && watch.outcome != WatchOutcome::Added {
        text.push_str(", added");
    }
    if let WatchOutcome::Recorded(Recorded {
        undone_at: Some(_), ..
    }) = watch.outcome
    {
        text.push_str(", undone");
    }
    if let Some(entry) = watch.moved_to {
        let _ = write!(text, ", moved to {entry}");
    }
    if let Some(entry) = watch.moved_from {
        let _ = write!(text, ", moved from {entry}");
    }
    text
}

/// `exact · entry 3`, or the confidence alone when nothing is linked.
fn match_text(link: &Link) -> String {
    match link.entry() {
        Some(entry) => format!("{} \u{b7} entry {entry}", link.confidence().tag()),
        None => link.confidence().tag().to_owned(),
    }
}

/// `HH:MM:SS.mmm LEVEL target message`, time of day in UTC.
pub fn event_line(event: &EventRecord) -> String {
    format!(
        "{} {:<5} {} {}",
        clock(event.at),
        event.level.to_string(),
        event.target,
        event.message
    )
}

/// `HH:MM:SS.mmm`, time of day in UTC.
fn clock(at: SystemTime) -> String {
    let since_epoch = at.duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = since_epoch.as_secs() % 86_400;
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        secs / 3600,
        (secs / 60) % 60,
        secs % 60,
        since_epoch.subsec_millis()
    )
}

fn source_label(source: DataDirSource) -> &'static str {
    match source {
        DataDirSource::PlatformDefault => "platform default",
        DataDirSource::EnvOverride => "RYUUJI_DATA_DIR",
        DataDirSource::Explicit => "explicit",
    }
}

fn source_text(source: DataDirSource) -> &'static str {
    match source {
        DataDirSource::PlatformDefault => "Windows default location",
        DataDirSource::EnvOverride => "Set by RYUUJI_DATA_DIR",
        DataDirSource::Explicit => "Given explicitly",
    }
}

fn schema_text(schema: &std::result::Result<SchemaVersion, ProbeFailed>) -> String {
    match schema {
        Ok(version) => version.to_string(),
        Err(ProbeFailed { detail }) => format!("unknown ({detail})"),
    }
}

fn schema_note(schema: &std::result::Result<SchemaVersion, ProbeFailed>) -> String {
    match schema {
        Ok(version) => format!("schema version {version}"),
        Err(_) => schema_text(schema),
    }
}

fn file_line(facts: &FileFacts, now: SystemTime) -> String {
    format!("{} ({})", facts.path.display(), stat_text(&facts.stat, now))
}

fn stat_text(stat: &FileStat, now: SystemTime) -> String {
    match stat {
        FileStat::Present { len, modified } => {
            format!("{len}, modified {}", modified_text(*modified, now))
        }
        other => size_text(other),
    }
}

fn size_text(stat: &FileStat) -> String {
    match stat {
        FileStat::Present { len, .. } => len.to_string(),
        FileStat::Missing => "missing".to_owned(),
        FileStat::Unreadable(kind) => format!("unreadable: {kind}"),
    }
}

fn modified_text(modified: Option<SystemTime>, now: SystemTime) -> String {
    let Some(modified) = modified else {
        return "at an unknown time".to_owned();
    };
    let unix = modified
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    format!("{unix} ({})", age_of(modified, now))
}

/// Writes `text` to the clipboard and describes the outcome for the page.
pub fn copy_to_clipboard(text: &str) -> String {
    match clipboard_win::set_clipboard_string(text) {
        Ok(()) => "Copied diagnostics".to_owned(),
        Err(err) => {
            warn!(%err, "clipboard write failed");
            format!("Copy failed: {err}")
        }
    }
}

/// The sources Diagnostics reads and the way back into the core. The page
/// gathers its own report from these on every render, so the refresh clock
/// it runs re-reads exactly what it shows; the level filter and the last
/// action's outcome are the view's own.
#[derive(Clone)]
pub struct DiagnosticsProps {
    pub dispatch: Dispatch<Command>,
    pub core: Rc<RefCell<Ryuuji>>,
    pub recent: RecentEvents,
    pub watcher: Rc<std::result::Result<Watcher, WatchError>>,
    pub syncer: Rc<std::result::Result<Syncer, SyncError>>,
}

/// Identity, not contents: two handles to the same core are the same
/// source. The reactor requires `PartialEq` on props; this is not a memo
/// key, and it never holds today because `dispatch` is fresh every render.
impl PartialEq for DiagnosticsProps {
    fn eq(&self, other: &DiagnosticsProps) -> bool {
        self.dispatch == other.dispatch
            && Rc::ptr_eq(&self.core, &other.core)
            && self.recent.shares_buffer(&other.recent)
            && Rc::ptr_eq(&self.watcher, &other.watcher)
            && Rc::ptr_eq(&self.syncer, &other.syncer)
    }
}

pub fn diagnostics(props: &DiagnosticsProps, cx: &mut RenderCx) -> Element {
    let (filter, set_filter) = cx.use_state(EventLevelFilter::All);
    let (last_action, set_last_action) = cx.use_state(None::<String>);
    ui::use_refresh(cx, true);

    let report = Report::gather(
        &props.core.borrow(),
        &props.recent,
        &props.watcher,
        &props.syncer,
    );
    let dispatch = props.dispatch.clone();
    let core = &report.core;
    let copy = {
        let report = report.clone();
        let set = set_last_action.clone();
        move || set.call(Some(copy_to_clipboard(&report.to_text())))
    };
    let inject = |status: PlaybackStatus, outcome: &'static str| {
        let dispatch = dispatch.clone();
        let set = set_last_action.clone();
        move || {
            dispatch.call(Command::Playback(injected(status)));
            set.call(Some(outcome.to_owned()));
        }
    };

    let actions_row = hstack((
        button("Copy diagnostics").accent().on_click(copy),
        caption(last_action.unwrap_or_default()).vertical_alignment(VerticalAlignment::Center),
    ))
    .spacing(12.0);

    let directory_card = card(
        Some(FOLDER_GLYPH),
        &core.data_dir.display().to_string(),
        format!(
            "{} · Ryuuji {APP_VERSION} · {BUILD_PROFILE} build",
            source_text(core.data_dir_source)
        ),
        open_folder_button(core.data_dir.clone(), set_last_action.clone()).into(),
    );

    let playback_card = card(
        None,
        "Inject playback event",
        "Feeds a canned event through Command::Playback, tagged injected.",
        hstack((
            button("Inject playing").on_click(inject(PlaybackStatus::Playing, "Injected playing")),
            button("Inject stopped").on_click(inject(PlaybackStatus::Stopped, "Injected stopped")),
        ))
        .spacing(8.0)
        .into(),
    );

    let events_header = grid((
        section("Recent events · newest first")
            .vertical_alignment(VerticalAlignment::Center)
            .grid_column(0),
        level_picker(filter, set_filter)
            .vertical_alignment(VerticalAlignment::Center)
            .grid_column(1),
    ))
    .columns([GridLength::STAR, GridLength::Auto]);

    scroll_viewer(
        vstack((
            actions_row,
            vstack((section("Data directory"), directory_card)).spacing(8.0),
            vstack((section("Files"), files_card(core, report.now))).spacing(8.0),
            vstack((section("Watches · newest first"), watches_card(&report))).spacing(8.0),
            vstack((section("Playback"), playback_card)).spacing(8.0),
            vstack((section("Media sessions"), sessions_card(&report.sessions))).spacing(8.0),
            vstack((section("Lists"), lists_cards(&report))).spacing(8.0),
            vstack((events_header, events_card(&report, filter))).spacing(8.0),
        ))
        .spacing(24.0)
        .max_width(CONTENT_MAX_WIDTH),
    )
    .into()
}

fn level_picker(filter: EventLevelFilter, set_filter: SetState<EventLevelFilter>) -> ComboBox {
    enum_picker(
        &EventLevelFilter::ALL,
        EventLevelFilter::label,
        filter,
        move |chosen| set_filter.call(chosen),
    )
    .min_width(200.0)
}

/// One line of the Files table.
struct FileRow {
    name: String,
    size: String,
    modified: String,
    note: String,
}

impl FileRow {
    fn of(facts: &FileFacts, root: &Path, note: String, now: SystemTime) -> FileRow {
        let modified = match facts.stat {
            FileStat::Present {
                modified: Some(modified),
                ..
            } => age_of(modified, now),
            FileStat::Present { modified: None, .. }
            | FileStat::Missing
            | FileStat::Unreadable(_) => NOT_APPLICABLE.to_owned(),
        };
        FileRow {
            name: relative(&facts.path, root),
            size: size_text(&facts.stat),
            modified,
            note,
        }
    }

    fn cells(self) -> [TextBlock; 4] {
        [
            text_block(self.name).selectable(),
            text_block(self.size),
            text_block(self.modified),
            text_block(self.note),
        ]
    }
}

fn files_card(core: &Diagnostics, now: SystemTime) -> Border {
    let root = core.data_dir.as_path();
    let log = match &core.current_log {
        Some(facts) => FileRow::of(facts, root, "current log".to_owned(), now),
        None => FileRow {
            name: format!("{}\\ (no file yet)", relative(&core.logs, root)),
            size: NOT_APPLICABLE.to_owned(),
            modified: NOT_APPLICABLE.to_owned(),
            note: NOT_APPLICABLE.to_owned(),
        },
    };
    let rows = [
        FileRow::of(&core.library, root, schema_note(&core.schema), now),
        FileRow::of(&core.settings, root, NOT_APPLICABLE.to_owned(), now),
        FileRow::of(&core.players, root, NOT_APPLICABLE.to_owned(), now),
        log,
    ];
    card_frame(
        table([
            GridLength::STAR,
            GridLength::Auto,
            GridLength::Auto,
            GridLength::Auto,
        ])
        .spacing(6.0, 16.0)
        .header(["Name", "Size", "Modified", "Note"])
        .rows(rows.into_iter().map(FileRow::cells)),
    )
}

fn watches_card(report: &Report) -> Border {
    let watches = match &report.core.watches {
        Ok(watches) if watches.is_empty() => return card_frame(caption("No watches yet.")),
        Ok(watches) => watches,
        Err(ProbeFailed { detail }) => {
            return card_frame(caption(format!("History unreadable: {detail}")));
        }
    };
    card_frame(
        table([
            GridLength::Auto,
            GridLength::Auto,
            GridLength::Auto,
            GridLength::Auto,
            GridLength::STAR,
        ])
        .spacing(6.0, 14.0)
        .header(["Id", "Opened", "Outcome", "Match", "Raw title"])
        .rows(watches.iter().map(|watch| watch_cells(watch, &report.zone))),
    )
}

fn watch_cells(watch: &Watch, zone: &TimeZone) -> [TextBlock; 5] {
    [
        mono(watch.id.to_string()).foreground(ThemeRef::SecondaryText),
        mono(date_time(watch.at, zone)).foreground(ThemeRef::SecondaryText),
        mono(outcome_text(watch)),
        mono(match_text(&watch.link)),
        mono(format!("{} \u{b7} {}", watch.raw_title, watch.player))
            .wrap()
            .selectable(),
    ]
}

fn sessions_card(sessions: &std::result::Result<Vec<SessionFacts>, String>) -> Border {
    let sessions = match sessions {
        Ok(sessions) if sessions.is_empty() => {
            return card_frame(caption("No media sessions."));
        }
        Ok(sessions) => sessions,
        Err(err) => return card_frame(caption(format!("Detection unavailable: {err}"))),
    };
    card_frame(
        table([
            GridLength::Auto,
            GridLength::STAR,
            GridLength::Auto,
            GridLength::Auto,
        ])
        .spacing(6.0, 16.0)
        .header(["App id", "Title", "Status", "Player"])
        .rows(sessions.iter().map(session_cells)),
    )
}

fn session_cells(session: &SessionFacts) -> [TextBlock; 4] {
    [
        mono(session.app_id.clone()).selectable(),
        text_block(session.title.clone()).wrap(),
        text_block(session.status.clone()),
        text_block(player_label(session)),
    ]
}

/// One card per service with the rows the report prints, under a card
/// saying why the worker is off when it is.
fn lists_cards(report: &Report) -> Element {
    let mut cards: Vec<Element> = Vec::new();
    if let Some(why) = &report.sync_down {
        cards.push(card_frame(caption(format!("Sync isn't running: {why}")).wrap()).into());
    }
    for list in &report.lists {
        let rows = list_rows(list, report.now, &report.zone)
            .into_iter()
            .map(|(label, value)| [caption(label), text_block(value).wrap().selectable()]);
        cards.push(
            card_frame(
                vstack((
                    text_block(list.service.label()).semibold(),
                    table([GridLength::Auto, GridLength::STAR])
                        .spacing(6.0, 16.0)
                        .rows(rows),
                ))
                .spacing(8.0),
            )
            .into(),
        );
    }
    vstack(cards).spacing(8.0).into()
}

/// `path` under `root`, or the whole path when it lives elsewhere.
fn relative(path: &Path, root: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}

fn events_card(report: &Report, filter: EventLevelFilter) -> Border {
    let events: Vec<&EventRecord> = report
        .events
        .iter()
        .rev()
        .filter(|event| filter.admits(event.level))
        .collect();
    if events.is_empty() {
        return card_frame(caption("Nothing at this level yet."));
    }
    card_frame(
        table([
            GridLength::Auto,
            GridLength::Auto,
            GridLength::Auto,
            GridLength::STAR,
        ])
        .spacing(4.0, 12.0)
        .rows(events.into_iter().map(event_cells)),
    )
}

fn event_cells(event: &EventRecord) -> [TextBlock; 4] {
    [
        mono(clock(event.at)).foreground(ThemeRef::SecondaryText),
        mono(event.level.to_string())
            .semibold()
            .foreground(level_brush(event.level)),
        mono(event.target.clone()).foreground(ThemeRef::SecondaryText),
        text_block(event.message.clone()).wrap(),
    ]
}

fn mono(text: impl Into<String>) -> TextBlock {
    text_block(text).font_family(MONO_FONT).font_size(12.0)
}

/// The status brush for a level word; the word itself carries the meaning.
fn level_brush(level: Level) -> ThemeRef {
    match level {
        Level::ERROR => ThemeRef::SystemCritical,
        Level::WARN => ThemeRef::SystemCaution,
        _ => ThemeRef::SecondaryText,
    }
}

#[cfg(test)]
mod tests {
    use ryuuji_core::{
        ByteSize, DataDir, Decline, Link, Media, MediaId, NewEntry, NewRecording, Opened,
        ProposedMatch, Store, UserId, WatchStatus,
    };
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    use super::*;

    fn record(level: Level, secs: u64, millis: u32, message: &str) -> EventRecord {
        EventRecord {
            at: UNIX_EPOCH + Duration::from_secs(secs) + Duration::from_millis(u64::from(millis)),
            level,
            target: "ryuuji_core::store".to_owned(),
            message: message.to_owned(),
        }
    }

    #[test]
    fn level_filter_admits_by_severity() {
        use EventLevelFilter::*;
        let levels = [
            Level::TRACE,
            Level::DEBUG,
            Level::INFO,
            Level::WARN,
            Level::ERROR,
        ];
        let admitted =
            |filter: EventLevelFilter| levels.iter().filter(|level| filter.admits(**level)).count();
        assert_eq!(admitted(All), 5);
        assert_eq!(admitted(WarnAndAbove), 2);
        assert_eq!(admitted(Error), 1);
        assert!(WarnAndAbove.admits(Level::WARN));
        assert!(!WarnAndAbove.admits(Level::INFO));
        assert!(Error.admits(Level::ERROR));
        assert!(!Error.admits(Level::WARN));
    }

    #[test]
    fn event_lines_carry_utc_time_level_target_and_message() {
        let event = record(Level::WARN, 1_756_339_200 + 3_723, 45, "library opened");
        assert_eq!(
            event_line(&event),
            "01:02:03.045 WARN  ryuuji_core::store library opened"
        );
        let event = record(Level::ERROR, 86_399, 999, "x");
        assert_eq!(
            event_line(&event),
            "23:59:59.999 ERROR ryuuji_core::store x"
        );
    }

    fn show(title: &str) -> NewEntry {
        NewEntry {
            title: title.to_owned(),
            status: WatchStatus::Watching,
            progress: 0,
            total: None,
            rewatching: false,
        }
    }

    fn recording(entry: ryuuji_core::EntryId, raw_title: &str) -> NewRecording {
        NewRecording {
            watch: None,
            entry,
            episode: 1..=1,
            raw_title: raw_title.to_owned(),
            parsed_title: "Show".to_owned(),
            player: "mpv".to_owned(),
        }
    }

    #[test]
    fn a_moved_pair_says_where_the_episode_went_and_came_from() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = DataDir::at(tmp.path()).unwrap();
        let Opened { mut store, .. } = Store::open(&dir).unwrap();
        let from = store.add(show("From")).unwrap().id;
        let to = store.add(show("To")).unwrap().id;
        let strayed = store
            .record(recording(from, "From - 01.mkv"))
            .unwrap()
            .watch
            .id;
        let moved = store.move_recording(strayed, to).unwrap();

        assert_eq!(
            outcome_text(&moved.undone),
            format!("recorded 0\u{2192}1, undone, moved to {to}")
        );
        assert_eq!(
            outcome_text(&moved.watch),
            format!("recorded 0\u{2192}1, moved from {from}")
        );
    }

    #[test]
    fn watch_outcomes_and_matches_read_in_the_stores_words() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = DataDir::at(tmp.path()).unwrap();
        let Opened { mut store, .. } = Store::open(&dir).unwrap();
        let proposal = |raw_title: &str, link| ProposedMatch {
            raw_title: raw_title.to_owned(),
            parsed_title: "Other".to_owned(),
            episode: Some(3..=3),
            season: None,
            release_group: None,
            link,
            player: "mpv".to_owned(),
            at: UNIX_EPOCH,
        };

        let a = store.add(show("A")).unwrap().id;
        store.record(recording(a, "A - 01.mkv")).unwrap();
        let unmatched = proposal("Other - 03.mkv", Link::Unmatched);
        let declined = store.decline(None, &unmatched, Decline::NotExact).unwrap();
        let added = store
            .add_to_library(show("Other"), Some(declined.id), &unmatched)
            .unwrap();
        let other = added.entry.id;
        store
            .decline(
                Some(added.watch.id),
                &proposal("Other - 03.mkv", Link::Exact(other)),
                Decline::NotNext,
            )
            .unwrap();
        let b = store.add(show("B")).unwrap().id;
        let undone = store.record(recording(b, "B - 01.mkv")).unwrap().watch.id;
        store.undo(undone).unwrap();
        let c = store
            .add_to_library(show("C"), None, &proposal("C - 03.mkv", Link::Unmatched))
            .unwrap()
            .entry
            .id;
        store
            .decline(
                None,
                &proposal("Z - 03.mkv", Link::Likely(b)),
                Decline::NotExact,
            )
            .unwrap();

        let watches = store.history(None, 10).unwrap().watches;
        let read: Vec<(String, String)> = watches
            .iter()
            .rev()
            .map(|watch| (outcome_text(watch), match_text(&watch.link)))
            .collect();
        let pair = |outcome: &str, link: String| (outcome.to_owned(), link);
        assert_eq!(
            read,
            vec![
                pair("recorded 0\u{2192}1", format!("exact \u{b7} entry {a}")),
                pair("not-next, added", format!("exact \u{b7} entry {other}")),
                pair(
                    "recorded 0\u{2192}1, undone",
                    format!("exact \u{b7} entry {b}")
                ),
                pair("added", format!("exact \u{b7} entry {c}")),
                pair("not-exact", format!("likely \u{b7} entry {b}")),
            ]
        );
    }

    #[test]
    fn report_text_contains_every_fact_and_ignores_the_view_filter() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = DataDir::at(tmp.path()).unwrap();
        let Opened { mut store, .. } = Store::open(&dir).unwrap();
        let entry = store.add(show("Show")).unwrap().id;
        store.record(recording(entry, "Show - 01.mkv")).unwrap();
        let core = Diagnostics::gather(&dir, &store);
        let events = vec![
            record(Level::INFO, 1_756_400_000, 0, "info event"),
            record(Level::ERROR, 1_756_400_001, 0, "error event"),
        ];
        let sessions = vec![
            SessionFacts {
                app_id: "mpv.exe".to_owned(),
                title: "Sousou no Frieren - 01".to_owned(),
                status: "Playing".to_owned(),
                player: Some("mpv".to_owned()),
                hidden: false,
            },
            SessionFacts {
                app_id: "Spotify.exe".to_owned(),
                title: String::new(),
                status: "Paused".to_owned(),
                player: None,
                hidden: false,
            },
        ];
        let last = ProposedMatch {
            raw_title: "[Subs] Show - 03.mkv".to_owned(),
            parsed_title: "Show".to_owned(),
            episode: Some(3..=3),
            season: None,
            release_group: Some("Subs".to_owned()),
            link: Link::Unmatched,
            player: "mpv".to_owned(),
            at: UNIX_EPOCH + Duration::from_secs(1_700_000_000),
        };
        let report = Report::new(
            core.clone(),
            Some(last),
            events,
            Ok(sessions),
            Vec::new(),
            None,
            TimeZone::UTC,
        );

        let text = report.to_text();
        let watch = &core.watches.as_ref().unwrap()[0];
        assert!(text.contains(&format!(
            "\n\nWatches (1):\n  #{} | {} | recorded 0\u{2192}1 | exact \u{b7} entry {entry} | \
             Show - 01.mkv | mpv\n\nMedia sessions:",
            watch.id,
            date_time(watch.at, &TimeZone::UTC)
        )));
        assert!(text.starts_with(&format!("Ryuuji {} (", env!("CARGO_PKG_VERSION"))));
        assert!(text.contains(&format!(
            "Data directory: {} [explicit]",
            dir.root().display()
        )));
        let FileStat::Present { len, .. } = &core.library.stat else {
            panic!("library present");
        };
        assert!(*len > ByteSize(0));
        assert!(text.contains(&format!(
            "Library: {} ({len}, modified ",
            core.library.path.display()
        )));
        assert!(text.contains("Schema version: 12\n"));
        assert!(text.contains(&format!(
            "Settings: {} (missing)",
            core.settings.path.display()
        )));
        assert!(text.contains(&format!("Logs: {}\n", dir.logs().display())));
        assert!(text.contains("Current log: none\n"));
        assert!(text.contains(
            "Last match:\n  raw: [Subs] Show - 03.mkv\n  title: Show\n  \
             episode: 3  season: \u{2014}  group: Subs\n  \
             entry: \u{2014}  confidence: unmatched\n  \
             player: mpv  at: 1700000000\n"
        ));
        assert!(text.contains(
            "Media sessions:\n  mpv.exe | Sousou no Frieren - 01 | Playing | mpv\n  \
             Spotify.exe |  | Paused | \u{2014}\n"
        ));
        assert!(text.contains("Recent events (2):\n"));
        assert!(text.contains(" INFO  ryuuji_core::store info event\n"));
        assert!(text.contains(" ERROR ryuuji_core::store error event\n"));
        assert!(text.contains(" ago)"));
    }

    #[test]
    fn gather_reads_the_core_the_buffer_and_the_watcher_outcome() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = DataDir::at(tmp.path()).unwrap();
        let core = Ryuuji::open(&dir).unwrap();
        let recent = RecentEvents::default();
        let _default = tracing_subscriber::registry()
            .with(recent.clone())
            .set_default();
        tracing::info!("gathered");

        let report = Report::gather(
            &core,
            &recent,
            &Err(WatchError::Exited("SMTC")),
            &Err(SyncError::Thread(std::io::Error::other("no thread"))),
        );

        assert_eq!(report.core, core.diagnostics());
        assert_eq!(report.last_match, core.state().last_match);
        assert_eq!(report.events[0].message, "gathered");
        assert_eq!(
            report.sessions,
            Err("the SMTC worker exited before reporting".to_owned())
        );
        assert_eq!(
            report.sync_down,
            Some("could not spawn the sync worker thread: no thread".to_owned())
        );
        assert_eq!(report.lists, list_facts(core.state(), &[]));
        assert_eq!(report.lists.len(), Service::ALL.len());
        assert_eq!(report.lists[0].lane, None);
    }

    #[test]
    fn report_text_names_empty_and_unavailable_sessions() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = DataDir::at(tmp.path()).unwrap();
        let Opened { store, .. } = Store::open(&dir).unwrap();
        let core = Diagnostics::gather(&dir, &store);

        let text = Report::new(
            core.clone(),
            None,
            Vec::new(),
            Ok(Vec::new()),
            Vec::new(),
            None,
            TimeZone::UTC,
        )
        .to_text();
        assert!(text.contains("Current log: none\nLast match: none\n\nWatches (0):\n\n"));
        assert!(text.contains("Media sessions:\n  none\n\nLists:\n\nRecent events (0):\n"));

        let text = Report::new(
            core,
            None,
            Vec::new(),
            Err("no manager".to_owned()),
            Vec::new(),
            None,
            TimeZone::UTC,
        )
        .to_text();
        assert!(text.contains("Media sessions:\n  unavailable: no manager\n"));
    }

    /// Whether `text` holds a word shaped like a JWT: three dot-separated
    /// runs of base64url characters, each eight or more long, so a version
    /// like `0.1.0` or a rolled log's name is not one.
    fn has_token_shape(text: &str) -> bool {
        text.split_whitespace().any(|word| {
            let segments: Vec<&str> = word.split('.').collect();
            segments.len() == 3
                && segments.iter().all(|segment| {
                    segment.len() >= 8
                        && segment.bytes().all(|byte| {
                            byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_'
                        })
                })
        })
    }

    #[test]
    fn report_text_lists_each_service_and_never_a_token() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = DataDir::at(tmp.path()).unwrap();
        let mut app = Ryuuji::open(&dir).unwrap();
        // 2026-09-23T00:00:00Z.
        let expires = UNIX_EPOCH + Duration::from_secs(1_790_121_600);
        let account = Account {
            service: Service::AniList,
            user: UserId(7),
            name: "someone".into(),
            expires: Some(expires),
            signed: Signed::In,
        };
        app.dispatch(Command::Synced(ryuuji_core::Report::SignedIn(
            account.clone(),
        )));
        for title in ["Frieren", "Dandadan"] {
            app.dispatch(Command::AddEntry(NewEntry {
                progress: 3,
                ..show(title)
            }));
        }
        let (frieren, dandadan) = (app.state().library[0].id, app.state().library[1].id);
        app.dispatch(Command::LinkShow {
            entry: frieren,
            service: Service::AniList,
            media: Media {
                id: MediaId(154587),
                title: "Sousou no Frieren".into(),
                episodes: Some(28),
                mal_id: None,
            },
        });
        app.dispatch(Command::StopSyncing {
            entry: dandadan,
            service: Service::AniList,
        });
        let lane = ServiceFacts {
            service: Service::AniList,
            stopped: None,
            paused: Some(Paused {
                why: Pause::Throttled,
                until: expires,
            }),
            rate_remaining: Some(89),
            last_call: Some(LastCall {
                at: expires,
                status: Some(200),
            }),
            due: 1,
            waiting: 2,
            running: 0,
            next_send: None,
        };
        let lists = list_facts(app.state(), std::slice::from_ref(&lane));
        assert_eq!(lists.len(), 1);
        assert_eq!(lists[0].account.as_ref(), Some(&account));
        assert_eq!(
            lists[0].shows,
            ShowCounts {
                linked: 1,
                waiting: 1,
                parked: 0,
                unpicked: 0,
                gone: 0,
                off: 1,
            }
        );

        let core = app.diagnostics();
        // The day before the token stops working.
        let mut report = Report::new(
            core.clone(),
            None,
            Vec::new(),
            Ok(Vec::new()),
            lists.clone(),
            None,
            TimeZone::UTC,
        );
        report.now = expires - Duration::from_secs(24 * 60 * 60);
        let text = report.to_text();
        assert!(
            text.contains(
                "Lists:\n  AniList\n    \
                 Standing: signed in as someone (user 7), token works until 2026-09-23 00:00\n    \
                 Paused: throttled until 2026-09-23 00:00\n    \
                 Rate remaining: 89\n    \
                 Last call: 2026-09-23 00:00 status 200\n    \
                 Jobs: 1 due, 2 waiting, 0 running\n    \
                 Shows: linked 1, waiting to send 1, parked 0, unpicked 0, gone 0, off 1\n\n\
                 Recent events (0):\n"
            ),
            "{text}"
        );
        assert!(!has_token_shape(&text), "{text}");

        // The token's time has come, and the core's row still says signed
        // in: the expiry is judged here.
        report.now = expires;
        let text = report.to_text();
        assert!(
            text.contains("  AniList\n    Standing: token expired\n    Paused:"),
            "{text}"
        );

        // The worker refused the kept token and the core has not heard
        // yet: the lane says so first.
        let stopped = list_facts(
            app.state(),
            &[ServiceFacts {
                stopped: Some(
                    "No AniList token is kept on this computer; paste one in Settings.".into(),
                ),
                ..lane.clone()
            }],
        );
        let text = Report::new(
            core.clone(),
            None,
            Vec::new(),
            Ok(Vec::new()),
            stopped,
            None,
            TimeZone::UTC,
        )
        .to_text();
        assert!(
            text.contains(
                "  AniList\n    Standing: refused: No AniList token is kept on this computer; \
                 paste one in Settings.\n    Paused:"
            ),
            "{text}"
        );
        // The scanner would have caught one, and passes the report's own
        // dotted words.
        assert!(has_token_shape(
            "Authorization: Bearer eyJhbGciOiJSUzI1NiIsInR5cCI6IkpXVCJ9.eyJleHAiOjE3OTAxMjE2MDB9.bm90IGEgcmVhbCBzaWduYXR1cmU"
        ));
        assert!(!has_token_shape(
            "Ryuuji 0.1.0 (debug) logs\\ryuuji.log.2026-09-24"
        ));

        // The worker off: the account's standing alone, and why.
        let mut report = Report::new(
            core.clone(),
            None,
            Vec::new(),
            Ok(Vec::new()),
            list_facts(app.state(), &[]),
            Some("could not spawn the sync worker thread: no thread".into()),
            TimeZone::UTC,
        );
        report.now = expires - Duration::from_secs(24 * 60 * 60);
        let text = report.to_text();
        assert!(
            text.contains(
                "Lists:\n  sync not running: could not spawn the sync worker thread: no thread\n  \
                 AniList\n    \
                 Standing: signed in as someone (user 7), token works until 2026-09-23 00:00\n    \
                 Shows: linked 1, waiting to send 1, parked 0, unpicked 0, gone 0, off 1\n"
            ),
            "{text}"
        );

        // Refused: the core's row says so while the worker, relaunched
        // since, has nothing stopped, and nothing waits to be sent.
        app.dispatch(Command::Synced(ryuuji_core::Report::Refused {
            service: Service::AniList,
            message: "Invalid token".into(),
        }));
        let refused = list_facts(app.state(), std::slice::from_ref(&lane));
        assert_eq!(refused[0].shows.waiting, 0);
        let text = Report::new(
            core,
            None,
            Vec::new(),
            Ok(Vec::new()),
            refused,
            None,
            TimeZone::UTC,
        )
        .to_text();
        assert!(
            text.contains("  AniList\n    Standing: refused: Invalid token\n    Paused:"),
            "{text}"
        );
    }

    #[test]
    fn relative_strips_the_root_and_keeps_foreign_paths() {
        let root = Path::new(r"C:\Users\me\Ryuuji");
        assert_eq!(
            relative(Path::new(r"C:\Users\me\Ryuuji\library.sqlite"), root),
            "library.sqlite"
        );
        assert_eq!(
            relative(
                Path::new(r"C:\Users\me\Ryuuji\logs\ryuuji.log.2026-08-29"),
                root
            ),
            r"logs\ryuuji.log.2026-08-29"
        );
        assert_eq!(
            relative(Path::new(r"D:\elsewhere\library.sqlite"), root),
            r"D:\elsewhere\library.sqlite"
        );
    }

    #[test]
    fn injected_events_carry_the_canned_values_and_the_injected_tag() {
        for status in [PlaybackStatus::Playing, PlaybackStatus::Stopped] {
            let event = injected(status);
            assert_eq!(event.title, INJECTED_TITLE);
            assert_eq!(event.player, INJECTED_PLAYER);
            assert_eq!(event.status, status);
            assert_eq!(event.position, Duration::from_secs(300));
            assert_eq!(event.duration, Duration::from_secs(1440));
            assert_eq!(event.source, PlaybackSource::Injected);
        }
    }

    #[test]
    fn level_brush_maps_warn_and_error() {
        assert_eq!(level_brush(Level::ERROR), ThemeRef::SystemCritical);
        assert_eq!(level_brush(Level::WARN), ThemeRef::SystemCaution);
        for level in [Level::INFO, Level::DEBUG, Level::TRACE] {
            assert_eq!(level_brush(level), ThemeRef::SecondaryText);
        }
    }
}
