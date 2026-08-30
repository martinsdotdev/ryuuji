//! The Diagnostics page: where the files are, what state they are in, and
//! the tail of the log, with the whole thing copyable as plain text.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command as Process;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ryuuji_core::{DataDirSource, Diagnostics, FileFacts, FileStat, ProbeFailed, SchemaVersion};
use tracing::{Level, warn};
use windows_reactor::*;

use crate::logging::EventRecord;

const MONO_FONT: &str = "Cascadia Mono";

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppInfo {
    pub version: &'static str,
    pub profile: &'static str,
}

impl AppInfo {
    fn current() -> AppInfo {
        AppInfo {
            version: env!("CARGO_PKG_VERSION"),
            profile: if cfg!(debug_assertions) {
                "debug"
            } else {
                "release"
            },
        }
    }
}

/// Everything the page shows, gathered at one moment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Report {
    pub app: AppInfo,
    pub core: Diagnostics,
    pub events: Vec<EventRecord>,
}

impl Report {
    pub fn new(core: Diagnostics, events: Vec<EventRecord>) -> Report {
        Report {
            app: AppInfo::current(),
            core,
            events,
        }
    }

    /// The plain-text form that goes to the clipboard. Every event is
    /// included whatever the page's level filter shows.
    pub fn to_text(&self) -> String {
        let now = SystemTime::now();
        let core = &self.core;
        let mut out = String::new();
        let _ = writeln!(out, "Ryuuji {} ({})", self.app.version, self.app.profile);
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
        let _ = writeln!(out);
        let _ = writeln!(out, "Recent events ({}):", self.events.len());
        for event in &self.events {
            let _ = writeln!(out, "{}", event_line(event));
        }
        out
    }
}

/// `HH:MM:SS.mmm LEVEL target message`, time of day in UTC.
pub fn event_line(event: &EventRecord) -> String {
    let since_epoch = event.at.duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = since_epoch.as_secs() % 86_400;
    format!(
        "{:02}:{:02}:{:02}.{:03} {:<5} {} {}",
        secs / 3600,
        (secs / 60) % 60,
        secs % 60,
        since_epoch.subsec_millis(),
        event.level.to_string(),
        event.target,
        event.message
    )
}

fn source_label(source: DataDirSource) -> &'static str {
    match source {
        DataDirSource::PlatformDefault => "platform default",
        DataDirSource::EnvOverride => "RYUUJI_DATA_DIR",
        DataDirSource::Explicit => "explicit",
    }
}

fn schema_text(schema: &std::result::Result<SchemaVersion, ProbeFailed>) -> String {
    match schema {
        Ok(version) => version.to_string(),
        Err(ProbeFailed { detail }) => format!("unknown ({detail})"),
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
        FileStat::Missing => "missing".to_owned(),
        FileStat::Unreadable(kind) => format!("unreadable: {kind}"),
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
    let age = now
        .duration_since(modified)
        .map_or_else(|_| "in the future".to_owned(), age_text);
    format!("{unix} ({age})")
}

fn age_text(age: Duration) -> String {
    let secs = age.as_secs();
    let amount = match secs {
        0..60 => format!("{secs} s"),
        60..3_600 => format!("{} min", secs / 60),
        3_600..86_400 => format!("{} h", secs / 3_600),
        _ => format!("{} d", secs / 86_400),
    };
    format!("{amount} ago")
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

/// Opens `root` in Explorer and describes the outcome for the page.
pub fn open_folder(root: &Path) -> String {
    match Process::new("explorer").arg(root).spawn() {
        Ok(_) => "Opened data folder".to_owned(),
        Err(err) => {
            warn!(%err, path = %root.display(), "could not open the data folder");
            format!("Could not open folder: {err}")
        }
    }
}

pub fn page(
    report: &Report,
    filter: EventLevelFilter,
    set_filter: SetState<EventLevelFilter>,
    last_action: Option<String>,
    set_last_action: SetState<Option<String>>,
) -> Element {
    let core = &report.core;
    let now = SystemTime::now();
    let events: Vec<Element> = report
        .events
        .iter()
        .filter(|event| filter.admits(event.level))
        .map(|event| {
            text_block(event_line(event))
                .font_family(MONO_FONT)
                .font_size(12.0)
                .selectable()
                .into()
        })
        .collect();

    scroll_viewer(
        vstack((
            actions(
                report.to_text(),
                core.data_dir.clone(),
                last_action,
                set_last_action,
            ),
            section(
                "Data directory",
                vec![
                    ("Path", core.data_dir.display().to_string()),
                    ("Source", source_label(core.data_dir_source).to_owned()),
                    ("Settings", core.settings.path.display().to_string()),
                    ("Settings state", stat_text(&core.settings.stat, now)),
                ],
            ),
            section(
                "Database",
                vec![
                    ("Path", core.library.path.display().to_string()),
                    ("Size", size_text(&core.library.stat)),
                    ("Modified", modified_only(&core.library.stat, now)),
                    ("Schema version", schema_text(&core.schema)),
                ],
            ),
            section(
                "Logs",
                vec![
                    ("Folder", core.logs.display().to_string()),
                    (
                        "Current file",
                        core.current_log.as_ref().map_or_else(
                            || "none".to_owned(),
                            |facts| facts.path.display().to_string(),
                        ),
                    ),
                    (
                        "Size",
                        core.current_log
                            .as_ref()
                            .map_or_else(|| "none".to_owned(), |facts| size_text(&facts.stat)),
                    ),
                    (
                        "Modified",
                        core.current_log.as_ref().map_or_else(
                            || "none".to_owned(),
                            |facts| modified_only(&facts.stat, now),
                        ),
                    ),
                ],
            ),
            text_block(format!(
                "Ryuuji {} ({} build)",
                report.app.version, report.app.profile
            ))
            .foreground(ThemeRef::SecondaryText),
            RadioButtons::new(EventLevelFilter::ALL.map(EventLevelFilter::label))
                .header("Recent events")
                .selected_index(selected_index(filter))
                .on_selection_changed(move |index: i32| {
                    let chosen = usize::try_from(index)
                        .ok()
                        .and_then(|index| EventLevelFilter::ALL.get(index));
                    if let Some(chosen) = chosen {
                        set_filter.call(*chosen);
                    }
                }),
            vstack(events).spacing(2.0),
        ))
        .spacing(12.0),
    )
    .into()
}

fn actions(
    text: String,
    root: PathBuf,
    last_action: Option<String>,
    set_last_action: SetState<Option<String>>,
) -> Element {
    let copy = {
        let set = set_last_action.clone();
        move || set.call(Some(copy_to_clipboard(&text)))
    };
    let open = move || set_last_action.call(Some(open_folder(&root)));
    hstack((
        button("Copy diagnostics").accent().on_click(copy),
        button("Open data folder").on_click(open),
        text_block(last_action.unwrap_or_default())
            .foreground(ThemeRef::SecondaryText)
            .vertical_alignment(VerticalAlignment::Center),
    ))
    .spacing(8.0)
    .into()
}

fn section(title: &str, rows: Vec<(&'static str, String)>) -> Element {
    let row_count = rows.len();
    let cells: Vec<Element> = rows
        .into_iter()
        .enumerate()
        .flat_map(|(index, (label, value))| {
            let row = i32::try_from(index).unwrap_or(i32::MAX);
            [
                text_block(label)
                    .foreground(ThemeRef::SecondaryText)
                    .grid_row(row)
                    .grid_column(0)
                    .into(),
                text_block(value)
                    .selectable()
                    .wrap()
                    .grid_row(row)
                    .grid_column(1)
                    .into(),
            ]
        })
        .collect();
    Expander::new(
        grid(cells)
            .rows(std::iter::repeat_n(GridLength::Auto, row_count))
            .columns([GridLength::Auto, GridLength::STAR])
            .row_spacing(4.0)
            .column_spacing(16.0),
    )
    .header(title)
    .expanded(true)
    .into()
}

fn modified_only(stat: &FileStat, now: SystemTime) -> String {
    match stat {
        FileStat::Present { modified, .. } => modified_text(*modified, now),
        FileStat::Missing => "missing".to_owned(),
        FileStat::Unreadable(kind) => format!("unreadable: {kind}"),
    }
}

fn selected_index(filter: EventLevelFilter) -> i32 {
    EventLevelFilter::ALL
        .iter()
        .position(|candidate| *candidate == filter)
        .and_then(|index| i32::try_from(index).ok())
        .unwrap_or(-1)
}

#[cfg(test)]
mod tests {
    use ryuuji_core::{ByteSize, DataDir, Opened, Store};

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
        let report = Report::new(core.clone(), events);

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
        assert!(text.contains("Schema version: 1\n"));
        assert!(text.contains(&format!(
            "Settings: {} (missing)",
            core.settings.path.display()
        )));
        assert!(text.contains(&format!("Logs: {}\n", dir.logs().display())));
        assert!(text.contains("Current log: none\n"));
        assert!(text.contains("Recent events (2):\n"));
        assert!(text.contains(" INFO  ryuuji_core::store info event\n"));
        assert!(text.contains(" ERROR ryuuji_core::store error event\n"));
        assert!(text.contains(" ago)"));
    }

    #[test]
    fn ages_round_to_the_largest_whole_unit() {
        assert_eq!(age_text(Duration::from_secs(12)), "12 s ago");
        assert_eq!(age_text(Duration::from_secs(150)), "2 min ago");
        assert_eq!(age_text(Duration::from_secs(7_200)), "2 h ago");
        assert_eq!(age_text(Duration::from_secs(200_000)), "2 d ago");
    }
}
