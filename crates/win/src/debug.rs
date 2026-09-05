//! The Diagnostics page: where the files are, what state they are in, and
//! the tail of the log, with the whole thing copyable as plain text.

use std::fmt::Write as _;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ryuuji_core::{
    Command, DataDirSource, Diagnostics, FileFacts, FileStat, PlaybackEvent, PlaybackSource,
    PlaybackStatus, ProbeFailed, ProposedMatch, SchemaVersion,
};
use ryuuji_detect::SessionFacts;
use tracing::{Level, warn};
use windows_reactor::*;

use crate::logging::EventRecord;
use crate::ui::{
    self, APP_VERSION, BUILD_PROFILE, CONTENT_MAX_WIDTH, FOLDER_GLYPH, age_of, caption, card,
    card_frame, enum_picker, open_folder, section, table,
};

const MONO_FONT: &str = "Cascadia Mono";
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
}

impl Report {
    pub fn new(
        core: Diagnostics,
        last_match: Option<ProposedMatch>,
        events: Vec<EventRecord>,
        sessions: std::result::Result<Vec<SessionFacts>, String>,
    ) -> Report {
        Report {
            now: SystemTime::now(),
            core,
            last_match,
            events,
            sessions,
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
                    count_text(last.episode),
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
        let _ = writeln!(out, "Recent events ({}):", self.events.len());
        for event in &self.events {
            let _ = writeln!(out, "{}", event_line(event));
        }
        out
    }
}

/// A count, or the em dash when absent.
fn count_text(value: Option<u32>) -> String {
    value.map_or_else(|| NOT_APPLICABLE.to_owned(), |n| n.to_string())
}

/// `app_id | title | status | player`, the player as [`NOT_APPLICABLE`] when
/// unmatched, matching the sessions card.
fn session_line(session: &SessionFacts) -> String {
    format!(
        "{} | {} | {} | {}",
        session.app_id,
        session.title,
        session.status,
        session.player.as_deref().unwrap_or(NOT_APPLICABLE)
    )
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

/// What Diagnostics shows and the way back into the core. The shell gathers
/// the report because it owns the core handle and the log buffer; the level
/// filter and the last action's outcome are the view's own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiagnosticsProps {
    pub dispatch: Dispatch<Command>,
    pub report: Report,
}

pub fn diagnostics(props: &DiagnosticsProps, cx: &mut RenderCx) -> Element {
    let (filter, set_filter) = cx.use_state(EventLevelFilter::All);
    let (last_action, set_last_action) = cx.use_state(None::<String>);
    ui::use_refresh(cx, true);

    let report = &props.report;
    let dispatch = props.dispatch.clone();
    let core = &report.core;
    let copy = {
        let text = report.to_text();
        let set = set_last_action.clone();
        move || set.call(Some(copy_to_clipboard(&text)))
    };
    let open = {
        let root = core.data_dir.clone();
        let set = set_last_action.clone();
        move || set.call(Some(open_folder(&root)))
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
        button("Open folder").on_click(open).into(),
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
            vstack((section("Playback"), playback_card)).spacing(8.0),
            vstack((section("Media sessions"), sessions_card(&report.sessions))).spacing(8.0),
            vstack((events_header, events_card(report, filter))).spacing(8.0),
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
        text_block(session.player.as_deref().unwrap_or(NOT_APPLICABLE)),
    ]
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
    use ryuuji_core::{ByteSize, DataDir, Link, Opened, ProposedMatch, Store};

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

    #[test]
    fn report_text_contains_every_fact_and_ignores_the_view_filter() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = DataDir::at(tmp.path()).unwrap();
        let Opened { store, .. } = Store::open(&dir).unwrap();
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
            },
            SessionFacts {
                app_id: "Spotify.exe".to_owned(),
                title: String::new(),
                status: "Paused".to_owned(),
                player: None,
            },
        ];
        let last = ProposedMatch {
            raw_title: "[Subs] Show - 03.mkv".to_owned(),
            parsed_title: "Show".to_owned(),
            episode: Some(3),
            season: None,
            release_group: Some("Subs".to_owned()),
            link: Link::Unmatched,
            player: "mpv".to_owned(),
            at: UNIX_EPOCH + Duration::from_secs(1_700_000_000),
        };
        let report = Report::new(core.clone(), Some(last), events, Ok(sessions));

        let text = report.to_text();
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
        assert!(text.contains("Schema version: 3\n"));
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
    fn report_text_names_empty_and_unavailable_sessions() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = DataDir::at(tmp.path()).unwrap();
        let Opened { store, .. } = Store::open(&dir).unwrap();
        let core = Diagnostics::gather(&dir, &store);

        let text = Report::new(core.clone(), None, Vec::new(), Ok(Vec::new())).to_text();
        assert!(text.contains("Current log: none\nLast match: none\n"));
        assert!(text.contains("Media sessions:\n  none\n\nRecent events (0):\n"));

        let text = Report::new(core, None, Vec::new(), Err("no manager".to_owned())).to_text();
        assert!(text.contains("Media sessions:\n  unavailable: no manager\n"));
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
