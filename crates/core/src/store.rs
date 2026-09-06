use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use std::{fmt, fs, io};

use rusqlite::ErrorCode;
use rusqlite::{Connection, OptionalExtension, Row, ToSql, params};
use rusqlite_migration::Migrations;
use tracing::{debug, info, info_span, warn};

use crate::{
    Confidence, DataDir, EntryId, LibraryEntry, Link, NewEntry, NewWatchEvent, ProposedMatch,
    WatchEvent, WatchEventId, WatchStatus,
};

/// The schema, compiled in from `migrations/`, one numbered directory per
/// version.
static MIGRATIONS: include_dir::Dir<'static> =
    include_dir::include_dir!("$CARGO_MANIFEST_DIR/migrations");

const SELECT_ENTRY: &str = "SELECT id, title, status, progress, total, rewatching FROM entries";

const SELECT_EVENT: &str = "SELECT id, entry_id, episode, episode_end, progress_before, progress, \
                            raw_title, player, at, undone_at FROM watch_events";

/// Files SQLite may keep beside the database; they move with it.
const SIDECAR_SUFFIXES: [&str; 3] = ["-journal", "-wal", "-shm"];

/// A database error whose concrete type stays inside this crate.
#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub struct DbError(DbErrorKind);

#[derive(Debug, thiserror::Error)]
enum DbErrorKind {
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Migration(#[from] rusqlite_migration::Error),
}

impl From<rusqlite::Error> for DbError {
    fn from(err: rusqlite::Error) -> Self {
        DbError(err.into())
    }
}

impl From<rusqlite_migration::Error> for DbError {
    fn from(err: rusqlite_migration::Error) -> Self {
        DbError(err.into())
    }
}

impl DbError {
    /// SQLite could not make sense of the file itself, as opposed to a
    /// query, lock or I/O failure on a sound database.
    fn is_corruption(&self) -> bool {
        let sqlite = match &self.0 {
            DbErrorKind::Sqlite(err) => err,
            DbErrorKind::Migration(rusqlite_migration::Error::RusqliteError { err, .. }) => err,
            DbErrorKind::Migration(_) => return false,
        };
        matches!(
            sqlite.sqlite_error_code(),
            Some(ErrorCode::NotADatabase | ErrorCode::DatabaseCorrupt)
        )
    }
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("could not open the library at {}", path.display())]
    Open {
        path: PathBuf,
        #[source]
        source: DbError,
    },
    #[error("could not migrate the library schema")]
    Migrate(#[source] DbError),
    #[error("library query failed")]
    Query(#[source] DbError),
    #[error("the library at {} failed its integrity check: {report}", path.display())]
    Corrupt { path: PathBuf, report: String },
    #[error("could not move the unreadable library {} aside to {}", path.display(), backup.display())]
    Quarantine {
        path: PathBuf,
        backup: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("no library entry with id {id}")]
    NotFound { id: EntryId },
    #[error("library entry {id} has an invalid {field}")]
    InvalidRow { id: EntryId, field: &'static str },
    #[error("the stored last match has an invalid {field}")]
    InvalidLastMatch { field: &'static str },
    #[error("watch event {id} has an invalid {field}")]
    InvalidWatchEvent {
        id: WatchEventId,
        field: &'static str,
    },
    /// One UPDATE guards both cases, so the store does not know which.
    #[error("watch event {id} is missing or already undone")]
    NothingToUndo { id: WatchEventId },
}

impl StoreError {
    fn is_corruption(&self) -> bool {
        match self {
            StoreError::Corrupt { .. } => true,
            StoreError::Open { source, .. }
            | StoreError::Migrate(source)
            | StoreError::Query(source) => source.is_corruption(),
            StoreError::Quarantine { .. }
            | StoreError::NotFound { .. }
            | StoreError::InvalidRow { .. }
            | StoreError::InvalidLastMatch { .. }
            | StoreError::InvalidWatchEvent { .. }
            | StoreError::NothingToUndo { .. } => false,
        }
    }
}

/// The library's schema version as SQLite stores it (`PRAGMA user_version`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SchemaVersion(pub(crate) i64);

impl fmt::Display for SchemaVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// A [`Store`] plus whether opening it had to move an unreadable file aside.
#[must_use]
pub struct Opened {
    pub store: Store,
    pub recovered: Option<Recovered>,
}

/// The library file was not a readable database and now lives at `backup`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Recovered {
    pub backup: PathBuf,
}

/// An entry and the watch event that last moved its progress, both as
/// stored after the same transaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Recording {
    pub entry: LibraryEntry,
    pub event: WatchEvent,
}

/// The on-disk library. Every mutation runs in one transaction and returns
/// the row as stored.
pub struct Store {
    conn: Connection,
}

impl Store {
    /// Opens or creates the library. A file SQLite cannot read is renamed to
    /// `library.sqlite.corrupt-<unix seconds>` (with any sidecars) and a
    /// fresh library takes its place; nothing is ever deleted.
    pub fn open(dir: &DataDir) -> Result<Opened, StoreError> {
        let path = dir.library_db();
        let _span = info_span!("store.open", path = %path.display()).entered();
        let failure = match try_open(&path) {
            Ok(store) => {
                info!("library opened");
                return Ok(Opened {
                    store,
                    recovered: None,
                });
            }
            Err(err) if err.is_corruption() => err,
            Err(err) => return Err(err),
        };
        let backup = with_suffix(&path, &format!(".corrupt-{}", unix_now()));
        warn!(
            error = %crate::error_chain(&failure),
            backup = %backup.display(),
            "library unreadable; moving it aside and starting fresh"
        );
        quarantine(&path, &backup)?;
        let store = try_open(&path)?;
        info!("library recreated");
        Ok(Opened {
            store,
            recovered: Some(Recovered { backup }),
        })
    }

    /// Every entry, ordered by id.
    pub fn entries(&self) -> Result<Vec<LibraryEntry>, StoreError> {
        let _span = info_span!("store.entries").entered();
        let mut stmt = self
            .conn
            .prepare(&format!("{SELECT_ENTRY} ORDER BY id"))
            .map_err(query_failed)?;
        let rows = stmt.query_map([], RawEntry::read).map_err(query_failed)?;
        let entries = rows
            .map(|row| row.map_err(query_failed)?.parse())
            .collect::<Result<Vec<_>, _>>()?;
        debug!(count = entries.len(), "entries loaded");
        Ok(entries)
    }

    pub fn add(&mut self, entry: NewEntry) -> Result<LibraryEntry, StoreError> {
        let _span = info_span!("store.add", title = %entry.title).entered();
        let tx = self.conn.transaction().map_err(query_failed)?;
        tx.execute(
            "INSERT INTO entries (title, status, progress, total, updated_at, rewatching) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                entry.title,
                entry.status.tag(),
                entry.progress,
                entry.total,
                unix_now(),
                entry.rewatching
            ],
        )
        .map_err(query_failed)?;
        let id = EntryId(tx.last_insert_rowid());
        let stored = fetch(&tx, id)?;
        tx.commit().map_err(query_failed)?;
        info!(id = %id, "entry added");
        Ok(stored)
    }

    pub fn set_progress(&mut self, id: EntryId, progress: u32) -> Result<LibraryEntry, StoreError> {
        let _span = info_span!("store.set_progress", id = %id, progress).entered();
        let stored = self.update(id, "progress", progress)?;
        info!("progress updated");
        Ok(stored)
    }

    pub fn set_status(
        &mut self,
        id: EntryId,
        status: WatchStatus,
    ) -> Result<LibraryEntry, StoreError> {
        let _span = info_span!("store.set_status", id = %id, status = status.tag()).entered();
        let stored = self.update(id, "status", status.tag())?;
        info!("status updated");
        Ok(stored)
    }

    fn update(
        &mut self,
        id: EntryId,
        column: &'static str,
        value: impl ToSql,
    ) -> Result<LibraryEntry, StoreError> {
        let tx = self.conn.transaction().map_err(query_failed)?;
        let stored = set_column(&tx, id, column, value)?;
        tx.commit().map_err(query_failed)?;
        Ok(stored)
    }

    /// Writes the progress a viewing earned and the row that says so, in one
    /// transaction, so neither can exist without the other. `at` is the
    /// write's own clock, like `updated_at`.
    pub fn record(&mut self, event: NewWatchEvent) -> Result<Recording, StoreError> {
        let _span = info_span!(
            "store.record",
            entry = %event.entry,
            episode = *event.episode.end()
        )
        .entered();
        let tx = self.conn.transaction().map_err(query_failed)?;
        let before = fetch(&tx, event.entry)?;
        let entry = set_column(&tx, event.entry, "progress", *event.episode.end())?;
        tx.execute(
            "INSERT INTO watch_events (entry_id, episode, episode_end, progress_before, \
             progress, raw_title, player, at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                event.entry.0,
                event.episode.start(),
                event.episode.end(),
                before.progress,
                entry.progress,
                event.raw_title,
                event.player,
                unix_now(),
            ],
        )
        .map_err(query_failed)?;
        let id = WatchEventId(tx.last_insert_rowid());
        let event = fetch_event(&tx, id)?;
        tx.commit().map_err(query_failed)?;
        info!(event = %id, progress = entry.progress, "progress recorded");
        Ok(Recording { entry, event })
    }

    /// Marks the event undone and puts its `progress_before` back on the
    /// entry, in one transaction. Restoring rather than decrementing is what
    /// takes a batch back to where it started. The row stays: nothing here
    /// deletes, and the mark is what stops a second undo.
    pub fn undo(&mut self, id: WatchEventId) -> Result<Recording, StoreError> {
        let _span = info_span!("store.undo", event = %id).entered();
        let tx = self.conn.transaction().map_err(query_failed)?;
        let changed = tx
            .execute(
                "UPDATE watch_events SET undone_at = ?1 WHERE id = ?2 AND undone_at IS NULL",
                params![unix_now(), id.0],
            )
            .map_err(query_failed)?;
        if changed == 0 {
            return Err(StoreError::NothingToUndo { id });
        }
        let event = fetch_event(&tx, id)?;
        let entry = set_column(&tx, event.entry, "progress", event.progress_before)?;
        tx.commit().map_err(query_failed)?;
        info!(entry = %entry.id, progress = entry.progress, "progress restored");
        Ok(Recording { entry, event })
    }

    /// Every watch event, oldest first, undone ones included.
    pub fn watch_events(&self) -> Result<Vec<WatchEvent>, StoreError> {
        let _span = info_span!("store.watch_events").entered();
        let mut stmt = self
            .conn
            .prepare(&format!("{SELECT_EVENT} ORDER BY id"))
            .map_err(query_failed)?;
        let rows = stmt
            .query_map([], RawWatchEvent::read)
            .map_err(query_failed)?;
        rows.map(|row| row.map_err(query_failed)?.parse()).collect()
    }

    /// Reads `PRAGMA user_version`; touches nothing on disk.
    pub fn schema_version(&self) -> Result<SchemaVersion, StoreError> {
        self.conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .map(SchemaVersion)
            .map_err(query_failed)
    }

    /// Writes the single last-match row, replacing whatever was there.
    pub fn save_last_match(&mut self, m: &ProposedMatch) -> Result<(), StoreError> {
        let _span = info_span!("store.save_last_match").entered();
        self.conn
            .execute(
                "INSERT OR REPLACE INTO last_match (id, raw_title, parsed_title, episode, \
                 season, release_group, entry_id, confidence, player, at, episode_end) \
                 VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    m.raw_title,
                    m.parsed_title,
                    m.episode.as_ref().map(|r| i64::from(*r.start())),
                    m.season.map(i64::from),
                    m.release_group,
                    m.link.entry().map(EntryId::as_i64),
                    m.link.confidence().tag(),
                    m.player,
                    unix_secs(m.at),
                    m.episode.as_ref().map(|r| i64::from(*r.end())),
                ],
            )
            .map_err(query_failed)?;
        info!(confidence = m.link.confidence().tag(), "last match saved");
        Ok(())
    }

    /// The stored last match, if any.
    pub fn last_match(&self) -> Result<Option<ProposedMatch>, StoreError> {
        let _span = info_span!("store.last_match").entered();
        self.conn
            .query_row(
                "SELECT raw_title, parsed_title, episode, season, release_group, \
                 entry_id, confidence, player, at, episode_end \
                 FROM last_match WHERE id = 1",
                [],
                RawLastMatch::read,
            )
            .optional()
            .map_err(query_failed)?
            .map(RawLastMatch::parse)
            .transpose()
    }

    #[cfg(test)]
    pub(crate) fn execute_raw(&self, sql: &str) {
        self.conn.execute(sql, []).unwrap();
    }
}

/// One attempt at opening `path`. The connection is a local, so every
/// failure path drops it before the caller can rename the file.
fn try_open(path: &Path) -> Result<Store, StoreError> {
    let mut conn = Connection::open(path).map_err(|err| StoreError::Open {
        path: path.to_path_buf(),
        source: err.into(),
    })?;
    Migrations::from_directory(&MIGRATIONS)
        .map_err(|err| StoreError::Migrate(err.into()))?
        .to_latest(&mut conn)
        .map_err(|err| StoreError::Migrate(err.into()))?;
    let report: String = conn
        .query_row("PRAGMA quick_check", [], |row| row.get(0))
        .map_err(query_failed)?;
    if report != "ok" {
        return Err(StoreError::Corrupt {
            path: path.to_path_buf(),
            report,
        });
    }
    conn.pragma_update(None, "synchronous", "FULL")
        .map_err(query_failed)?;
    conn.pragma_update(None, "foreign_keys", "ON")
        .map_err(query_failed)?;
    Ok(Store { conn })
}

fn quarantine(path: &Path, backup: &Path) -> Result<(), StoreError> {
    let rename = |from: &Path, to: &Path| {
        fs::rename(from, to).map_err(|source| StoreError::Quarantine {
            path: from.to_path_buf(),
            backup: to.to_path_buf(),
            source,
        })
    };
    rename(path, backup)?;
    for suffix in SIDECAR_SUFFIXES {
        let sidecar = with_suffix(path, suffix);
        if sidecar.exists() {
            rename(&sidecar, &with_suffix(backup, suffix))?;
        }
    }
    Ok(())
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

fn fetch(conn: &Connection, id: EntryId) -> Result<LibraryEntry, StoreError> {
    conn.query_row(
        &format!("{SELECT_ENTRY} WHERE id = ?1"),
        [id.0],
        RawEntry::read,
    )
    .optional()
    .map_err(query_failed)?
    .ok_or(StoreError::NotFound { id })?
    .parse()
}

/// One column of one entry, stamped with `updated_at`, read back as stored.
/// Runs on the caller's connection so a transaction can hold more than one
/// write.
fn set_column(
    conn: &Connection,
    id: EntryId,
    column: &'static str,
    value: impl ToSql,
) -> Result<LibraryEntry, StoreError> {
    let changed = conn
        .execute(
            &format!("UPDATE entries SET {column} = ?1, updated_at = ?2 WHERE id = ?3"),
            params![value, unix_now(), id.0],
        )
        .map_err(query_failed)?;
    if changed == 0 {
        return Err(StoreError::NotFound { id });
    }
    fetch(conn, id)
}

fn fetch_event(conn: &Connection, id: WatchEventId) -> Result<WatchEvent, StoreError> {
    conn.query_row(
        &format!("{SELECT_EVENT} WHERE id = ?1"),
        [id.0],
        RawWatchEvent::read,
    )
    .map_err(query_failed)?
    .parse()
}

/// A row as SQLite hands it over, before the domain checks in `parse`.
struct RawEntry {
    id: i64,
    title: String,
    status: String,
    progress: i64,
    total: Option<i64>,
    rewatching: bool,
}

impl RawEntry {
    // Positional, so `SELECT_ENTRY` only ever appends a column.
    fn read(row: &Row<'_>) -> rusqlite::Result<RawEntry> {
        Ok(RawEntry {
            id: row.get(0)?,
            title: row.get(1)?,
            status: row.get(2)?,
            progress: row.get(3)?,
            total: row.get(4)?,
            rewatching: row.get(5)?,
        })
    }

    fn parse(self) -> Result<LibraryEntry, StoreError> {
        let id = EntryId(self.id);
        let invalid = |field| StoreError::InvalidRow { id, field };
        Ok(LibraryEntry {
            id,
            title: self.title,
            status: WatchStatus::from_tag(&self.status).ok_or_else(|| invalid("status"))?,
            progress: u32::try_from(self.progress).map_err(|_| invalid("progress"))?,
            total: self
                .total
                .map(u32::try_from)
                .transpose()
                .map_err(|_| invalid("total"))?,
            rewatching: self.rewatching,
        })
    }
}

/// The last-match row as SQLite hands it over, before the domain checks in
/// `parse`.
struct RawLastMatch {
    raw_title: String,
    parsed_title: String,
    episode: Option<i64>,
    season: Option<i64>,
    release_group: Option<String>,
    entry_id: Option<i64>,
    confidence: String,
    player: String,
    at: i64,
    episode_end: Option<i64>,
}

impl RawLastMatch {
    // Positional, so the SELECT in `last_match` only ever appends a column.
    fn read(row: &Row<'_>) -> rusqlite::Result<RawLastMatch> {
        Ok(RawLastMatch {
            raw_title: row.get(0)?,
            parsed_title: row.get(1)?,
            episode: row.get(2)?,
            season: row.get(3)?,
            release_group: row.get(4)?,
            entry_id: row.get(5)?,
            confidence: row.get(6)?,
            player: row.get(7)?,
            at: row.get(8)?,
            episode_end: row.get(9)?,
        })
    }

    fn parse(self) -> Result<ProposedMatch, StoreError> {
        let invalid = |field| StoreError::InvalidLastMatch { field };
        Ok(ProposedMatch {
            raw_title: self.raw_title,
            parsed_title: self.parsed_title,
            episode: episode_range(self.episode, self.episode_end)
                .map_err(|()| invalid("episode"))?,
            season: self
                .season
                .map(u32::try_from)
                .transpose()
                .map_err(|_| invalid("season"))?,
            release_group: self.release_group,
            link: Link::from_columns(
                self.entry_id.map(EntryId),
                Confidence::from_tag(&self.confidence).ok_or_else(|| invalid("confidence"))?,
            )
            .ok_or_else(|| invalid("link"))?,
            player: self.player,
            at: UNIX_EPOCH
                + Duration::from_secs(u64::try_from(self.at).map_err(|_| invalid("at"))?),
        })
    }
}

/// A watch-event row as SQLite hands it over, before the domain checks in
/// `parse`.
struct RawWatchEvent {
    id: i64,
    entry_id: i64,
    episode: i64,
    episode_end: i64,
    progress_before: i64,
    progress: i64,
    raw_title: String,
    player: String,
    at: i64,
    undone_at: Option<i64>,
}

impl RawWatchEvent {
    // Positional, so `SELECT_EVENT` only ever appends a column.
    fn read(row: &Row<'_>) -> rusqlite::Result<RawWatchEvent> {
        Ok(RawWatchEvent {
            id: row.get(0)?,
            entry_id: row.get(1)?,
            episode: row.get(2)?,
            episode_end: row.get(3)?,
            progress_before: row.get(4)?,
            progress: row.get(5)?,
            raw_title: row.get(6)?,
            player: row.get(7)?,
            at: row.get(8)?,
            undone_at: row.get(9)?,
        })
    }

    fn parse(self) -> Result<WatchEvent, StoreError> {
        let id = WatchEventId(self.id);
        let invalid = |field| StoreError::InvalidWatchEvent { id, field };
        let progress = |value: i64, field| u32::try_from(value).map_err(|_| invalid(field));
        Ok(WatchEvent {
            id,
            entry: EntryId(self.entry_id),
            episode: match episode_range(Some(self.episode), Some(self.episode_end)) {
                Ok(Some(range)) => range,
                Ok(None) | Err(()) => return Err(invalid("episode")),
            },
            progress_before: progress(self.progress_before, "progress_before")?,
            progress: progress(self.progress, "progress")?,
            raw_title: self.raw_title,
            player: self.player,
            at: unix_time(self.at).ok_or_else(|| invalid("at"))?,
            undone_at: self
                .undone_at
                .map(|at| unix_time(at).ok_or_else(|| invalid("undone_at")))
                .transpose()?,
        })
    }
}

/// The two episode columns read as one value: both NULL is no episode, both
/// set and ordered is the span, anything else is a broken row.
fn episode_range(low: Option<i64>, high: Option<i64>) -> Result<Option<RangeInclusive<u32>>, ()> {
    match (low, high) {
        (None, None) => Ok(None),
        (Some(low), Some(high)) => {
            let low = u32::try_from(low).map_err(|_| ())?;
            let high = u32::try_from(high).map_err(|_| ())?;
            if low <= high {
                Ok(Some(low..=high))
            } else {
                Err(())
            }
        }
        (None, Some(_)) | (Some(_), None) => Err(()),
    }
}

fn query_failed(err: rusqlite::Error) -> StoreError {
    StoreError::Query(err.into())
}

fn unix_now() -> i64 {
    unix_secs(SystemTime::now())
}

fn unix_secs(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH)
        .map(|elapsed| i64::try_from(elapsed.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// Inverse of [`unix_secs`]; a negative stamp is a broken row.
fn unix_time(secs: i64) -> Option<SystemTime> {
    u64::try_from(secs)
        .ok()
        .map(|secs| UNIX_EPOCH + Duration::from_secs(secs))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_tmp() -> (tempfile::TempDir, DataDir) {
        let tmp = tempfile::tempdir().unwrap();
        let dir = DataDir::at(tmp.path()).unwrap();
        (tmp, dir)
    }

    fn open(dir: &DataDir) -> Store {
        let opened = Store::open(dir).unwrap();
        assert_eq!(opened.recovered, None);
        opened.store
    }

    fn backup_name(recovered: &Recovered) -> String {
        recovered
            .backup
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned()
    }

    fn assert_backup_name(name: &str) {
        let stamp = name
            .strip_prefix("library.sqlite.corrupt-")
            .unwrap_or_else(|| panic!("unexpected backup name {name}"));
        assert!(!stamp.is_empty() && stamp.bytes().all(|b| b.is_ascii_digit()));
    }

    fn entry(title: &str) -> NewEntry {
        NewEntry {
            title: title.into(),
            status: WatchStatus::Watching,
            progress: 0,
            total: None,
            rewatching: false,
        }
    }

    fn schema_version(store: &Store) -> SchemaVersion {
        store.schema_version().unwrap()
    }

    #[test]
    fn open_creates_the_database_at_schema_v5_without_recovery() {
        let (_tmp, dir) = open_tmp();
        let store = open(&dir);
        assert!(dir.root().join("library.sqlite").is_file());
        assert_eq!(schema_version(&store), SchemaVersion(5));
        drop(store);
        assert_eq!(schema_version(&open(&dir)), SchemaVersion(5));
    }

    #[test]
    fn schema_version_reads_the_user_version_pragma() {
        let (_tmp, dir) = open_tmp();
        let store = open(&dir);
        store.execute_raw("PRAGMA user_version = 42");
        let pragma: i64 = store
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(pragma, 42);
        assert_eq!(schema_version(&store), SchemaVersion(42));
        assert_eq!(schema_version(&store).to_string(), "42");
    }

    #[test]
    fn garbage_file_is_moved_aside_and_replaced() {
        let (_tmp, dir) = open_tmp();
        let garbage = b"this is not a database";
        fs::write(dir.library_db(), garbage).unwrap();

        let Opened { store, recovered } = Store::open(&dir).unwrap();
        let recovered = recovered.expect("recovery reported");
        assert_backup_name(&backup_name(&recovered));
        assert_eq!(fs::read(&recovered.backup).unwrap(), garbage);
        assert_eq!(schema_version(&store), SchemaVersion(5));
        assert_eq!(store.entries().unwrap(), vec![]);
        drop(store);

        assert_eq!(Store::open(&dir).unwrap().recovered, None);
    }

    // SQLite unlinks a journal it does not recognise while opening the main
    // file, so this exercises the rename directly rather than through `open`.
    #[test]
    fn quarantine_moves_every_sidecar_and_deletes_nothing() {
        let (_tmp, dir) = open_tmp();
        let path = dir.library_db();
        fs::write(&path, b"garbage").unwrap();
        for suffix in SIDECAR_SUFFIXES {
            fs::write(with_suffix(&path, suffix), suffix).unwrap();
        }
        let backup = with_suffix(&path, ".corrupt-0");

        quarantine(&path, &backup).unwrap();

        assert!(!path.exists());
        assert_eq!(fs::read(&backup).unwrap(), b"garbage");
        for suffix in SIDECAR_SUFFIXES {
            assert!(!with_suffix(&path, suffix).exists());
            assert_eq!(
                fs::read(with_suffix(&backup, suffix)).unwrap(),
                suffix.as_bytes()
            );
        }
        assert_eq!(fs::read_dir(dir.root()).unwrap().count(), 5);
    }

    #[test]
    fn add_returns_stored_rows_with_increasing_ids() {
        let (_tmp, dir) = open_tmp();
        let mut store = open(&dir);
        let first = store.add(entry("First")).unwrap();
        let second = store.add(entry("Second")).unwrap();
        assert_eq!(first.title, "First");
        assert!(second.id > first.id);
        assert_eq!(store.entries().unwrap(), vec![first, second]);
    }

    #[test]
    fn mutations_persist_across_reopen() {
        let (_tmp, dir) = open_tmp();
        let mut store = open(&dir);
        let id = store.add(entry("Show")).unwrap().id;
        let after_progress = store.set_progress(id, 7).unwrap();
        assert_eq!(after_progress.progress, 7);
        let after_status = store.set_status(id, WatchStatus::OnHold).unwrap();
        assert_eq!(after_status.status, WatchStatus::OnHold);
        drop(store);

        let reopened = open(&dir);
        assert_eq!(reopened.entries().unwrap(), vec![after_status]);
    }

    #[test]
    fn set_progress_on_unknown_id_is_not_found_and_changes_nothing() {
        let (_tmp, dir) = open_tmp();
        let mut store = open(&dir);
        let existing = store.add(entry("Show")).unwrap();
        let unknown = EntryId(existing.id.0 + 100);
        assert!(matches!(
            store.set_progress(unknown, 3),
            Err(StoreError::NotFound { id }) if id == unknown
        ));
        assert_eq!(store.entries().unwrap(), vec![existing]);
    }

    #[test]
    fn garbage_status_surfaces_as_invalid_row() {
        let (_tmp, dir) = open_tmp();
        let store = open(&dir);
        store.execute_raw(
            "INSERT INTO entries (title, status, progress, updated_at) VALUES ('x', 'bogus', 0, 0)",
        );
        assert!(matches!(
            store.entries(),
            Err(StoreError::InvalidRow {
                field: "status",
                ..
            })
        ));
    }

    fn proposed(raw: &str) -> ProposedMatch {
        ProposedMatch {
            raw_title: raw.into(),
            parsed_title: "Show".into(),
            episode: Some(3..=3),
            season: Some(2),
            release_group: Some("Subs".into()),
            link: Link::Unmatched,
            player: "mpv".into(),
            at: UNIX_EPOCH + Duration::from_secs(1_700_000_000),
        }
    }

    #[test]
    fn v1_library_migrates_to_v5_and_keeps_entries() {
        let (_tmp, dir) = open_tmp();
        let conn = Connection::open(dir.library_db()).unwrap();
        conn.execute_batch(include_str!("../migrations/01-entries/up.sql"))
            .unwrap();
        conn.execute(
            "INSERT INTO entries (title, status, progress, total, updated_at) \
             VALUES ('Show', 'watching', 3, 12, 0)",
            [],
        )
        .unwrap();
        conn.pragma_update(None, "user_version", 1).unwrap();
        drop(conn);

        let store = open(&dir);
        assert_eq!(schema_version(&store), SchemaVersion(5));
        let entries = store.entries().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].title, "Show");
        assert_eq!(entries[0].progress, 3);
        assert_eq!(entries[0].total, Some(12));
        assert!(!entries[0].rewatching);
        assert_eq!(store.last_match().unwrap(), None);
    }

    #[test]
    fn v2_last_match_survives_the_outcome_drop() {
        let (_tmp, dir) = open_tmp();
        let conn = Connection::open(dir.library_db()).unwrap();
        conn.execute_batch(include_str!("../migrations/01-entries/up.sql"))
            .unwrap();
        conn.execute_batch(include_str!("../migrations/02-last_match/up.sql"))
            .unwrap();
        conn.execute(
            "INSERT INTO last_match (id, raw_title, parsed_title, episode, season, \
             release_group, entry_id, confidence, outcome, player, at) \
             VALUES (1, 'Show - 03.mkv', 'Show', 3, 2, 'Subs', NULL, 'unmatched', \
             'proposed', 'mpv', 1700000000)",
            [],
        )
        .unwrap();
        conn.pragma_update(None, "user_version", 2).unwrap();
        drop(conn);

        let store = open(&dir);
        assert_eq!(schema_version(&store), SchemaVersion(5));
        assert_eq!(store.last_match().unwrap(), Some(proposed("Show - 03.mkv")));
    }

    #[test]
    fn v3_last_match_gains_an_episode_end() {
        let (_tmp, dir) = open_tmp();
        let conn = Connection::open(dir.library_db()).unwrap();
        for sql in [
            include_str!("../migrations/01-entries/up.sql"),
            include_str!("../migrations/02-last_match/up.sql"),
            include_str!("../migrations/03-drop_outcome/up.sql"),
        ] {
            conn.execute_batch(sql).unwrap();
        }
        conn.execute(
            "INSERT INTO last_match (id, raw_title, parsed_title, episode, season, \
             release_group, entry_id, confidence, player, at) \
             VALUES (1, 'Show - 03.mkv', 'Show', 3, 2, 'Subs', NULL, 'unmatched', \
             'mpv', 1700000000)",
            [],
        )
        .unwrap();
        conn.pragma_update(None, "user_version", 3).unwrap();
        drop(conn);

        let store = open(&dir);
        assert_eq!(schema_version(&store), SchemaVersion(5));
        assert_eq!(store.last_match().unwrap(), Some(proposed("Show - 03.mkv")));
    }

    #[test]
    fn v4_library_migrates_to_v5_with_an_empty_history() {
        let (_tmp, dir) = open_tmp();
        let conn = Connection::open(dir.library_db()).unwrap();
        for sql in [
            include_str!("../migrations/01-entries/up.sql"),
            include_str!("../migrations/02-last_match/up.sql"),
            include_str!("../migrations/03-drop_outcome/up.sql"),
            include_str!("../migrations/04-episode_range_and_rewatching/up.sql"),
        ] {
            conn.execute_batch(sql).unwrap();
        }
        conn.execute(
            "INSERT INTO entries (id, title, status, progress, total, updated_at) \
             VALUES (7, 'Show', 'watching', 3, 12, 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO last_match (id, raw_title, parsed_title, episode, season, \
             release_group, entry_id, confidence, player, at, episode_end) \
             VALUES (1, 'Show - 03.mkv', 'Show', 3, 2, 'Subs', 7, 'exact', \
             'mpv', 1700000000, 3)",
            [],
        )
        .unwrap();
        conn.pragma_update(None, "user_version", 4).unwrap();
        drop(conn);

        let store = open(&dir);
        assert_eq!(schema_version(&store), SchemaVersion(5));
        let entries = store.entries().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, EntryId(7));
        assert_eq!(entries[0].progress, 3);
        assert_eq!(
            store.last_match().unwrap(),
            Some(ProposedMatch {
                link: Link::Exact(EntryId(7)),
                ..proposed("Show - 03.mkv")
            })
        );
        assert_eq!(store.watch_events().unwrap(), vec![]);
    }

    fn watching(id: EntryId, episode: RangeInclusive<u32>) -> NewWatchEvent {
        NewWatchEvent {
            entry: id,
            episode,
            raw_title: "Show - 03.mkv".into(),
            player: "mpv".into(),
        }
    }

    #[test]
    fn record_writes_progress_and_its_row_together() {
        let (_tmp, dir) = open_tmp();
        let mut store = open(&dir);
        let id = store.add(entry("Show")).unwrap().id;
        store.set_progress(id, 2).unwrap();

        let Recording { entry, event } = store.record(watching(id, 3..=4)).unwrap();
        assert_eq!(entry.progress, 4);
        assert_eq!(event.entry, id);
        assert_eq!(event.episode, 3..=4);
        assert_eq!(event.progress_before, 2);
        assert_eq!(event.progress, 4);
        assert_eq!(event.raw_title, "Show - 03.mkv");
        assert_eq!(event.player, "mpv");
        assert!(event.at >= UNIX_EPOCH + Duration::from_secs(1_700_000_000));
        assert_eq!(event.undone_at, None);

        let second = store.record(watching(id, 5..=5)).unwrap();
        assert!(second.event.id > event.id);
        assert_eq!(second.event.progress_before, 4);
        drop(store);

        let reopened = open(&dir);
        assert_eq!(reopened.entries().unwrap(), vec![second.entry]);
        assert_eq!(reopened.watch_events().unwrap(), vec![event, second.event]);
    }

    #[test]
    fn record_on_unknown_id_is_not_found_and_inserts_nothing() {
        let (_tmp, dir) = open_tmp();
        let mut store = open(&dir);
        let existing = store.add(entry("Show")).unwrap();
        let unknown = EntryId(existing.id.0 + 100);
        assert!(matches!(
            store.record(watching(unknown, 1..=1)),
            Err(StoreError::NotFound { id }) if id == unknown
        ));
        assert_eq!(store.entries().unwrap(), vec![existing]);
        assert_eq!(store.watch_events().unwrap(), vec![]);
    }

    #[test]
    fn undo_restores_progress_before_and_marks_the_row() {
        let (_tmp, dir) = open_tmp();
        let mut store = open(&dir);
        let id = store.add(entry("Show")).unwrap().id;
        store.set_progress(id, 2).unwrap();
        let recorded = store.record(watching(id, 3..=5)).unwrap();
        assert_eq!(recorded.entry.progress, 5);

        let Recording { entry, event } = store.undo(recorded.event.id).unwrap();
        assert_eq!(entry.progress, 2);
        assert!(event.undone_at.is_some());
        assert_eq!(
            event,
            WatchEvent {
                undone_at: event.undone_at,
                ..recorded.event
            }
        );
        drop(store);

        let reopened = open(&dir);
        assert_eq!(reopened.entries().unwrap(), vec![entry]);
        assert_eq!(reopened.watch_events().unwrap(), vec![event]);
    }

    #[test]
    fn undo_refuses_an_undone_or_unknown_event_and_changes_nothing() {
        let (_tmp, dir) = open_tmp();
        let mut store = open(&dir);
        let id = store.add(entry("Show")).unwrap().id;
        let event = store.record(watching(id, 1..=1)).unwrap().event.id;
        let undone = store.undo(event).unwrap();
        let moved_on = store.set_progress(id, 4).unwrap();

        assert!(matches!(
            store.undo(event),
            Err(StoreError::NothingToUndo { id }) if id == event
        ));
        let unknown = WatchEventId(event.0 + 100);
        assert!(matches!(
            store.undo(unknown),
            Err(StoreError::NothingToUndo { id }) if id == unknown
        ));
        assert_eq!(store.entries().unwrap(), vec![moved_on]);
        assert_eq!(store.watch_events().unwrap(), vec![undone.event]);
    }

    #[test]
    fn garbage_watch_event_surfaces_as_invalid() {
        let (_tmp, dir) = open_tmp();
        let mut store = open(&dir);
        let id = store.add(entry("Show")).unwrap().id;
        let event = store.record(watching(id, 1..=1)).unwrap().event.id;
        for (broken, field) in [
            (
                "UPDATE watch_events SET episode = 5, episode_end = 2",
                "episode",
            ),
            (
                "UPDATE watch_events SET episode = 1, episode_end = 1, progress = -1",
                "progress",
            ),
            (
                "UPDATE watch_events SET progress = 1, undone_at = -5",
                "undone_at",
            ),
        ] {
            store.execute_raw(broken);
            assert!(
                matches!(
                    store.watch_events(),
                    Err(StoreError::InvalidWatchEvent { id: got, field: got_field })
                        if got == event && got_field == field
                ),
                "{broken}"
            );
        }
    }

    #[test]
    fn disagreeing_episode_columns_are_invalid() {
        let (_tmp, dir) = open_tmp();
        let mut store = open(&dir);
        store.save_last_match(&proposed("Show - 03.mkv")).unwrap();
        for broken in [
            "UPDATE last_match SET episode = 5, episode_end = 2",
            "UPDATE last_match SET episode = 3, episode_end = NULL",
            "UPDATE last_match SET episode = NULL, episode_end = 3",
            "UPDATE last_match SET episode = -1, episode_end = 3",
        ] {
            store.execute_raw(broken);
            assert!(
                matches!(
                    store.last_match(),
                    Err(StoreError::InvalidLastMatch { field: "episode" })
                ),
                "{broken}"
            );
        }
    }

    #[test]
    fn last_match_round_trips_a_batch() {
        let (_tmp, dir) = open_tmp();
        let mut store = open(&dir);
        let batch = ProposedMatch {
            episode: Some(1..=12),
            ..proposed("Show - 01-12.mkv")
        };
        store.save_last_match(&batch).unwrap();
        assert_eq!(store.last_match().unwrap(), Some(batch));
    }

    #[test]
    fn add_persists_the_rewatch_flag() {
        let (_tmp, dir) = open_tmp();
        let mut store = open(&dir);
        let again = store
            .add(NewEntry {
                rewatching: true,
                ..entry("Again")
            })
            .unwrap();
        let fresh = store.add(entry("Fresh")).unwrap();
        assert!(again.rewatching);
        assert!(!fresh.rewatching);
        assert_eq!(store.entries().unwrap(), vec![again, fresh]);
    }

    #[test]
    fn migrations_directory_is_well_formed() {
        Migrations::from_directory(&MIGRATIONS)
            .unwrap()
            .validate()
            .unwrap();
    }

    #[test]
    fn last_match_is_none_until_saved() {
        let (_tmp, dir) = open_tmp();
        assert_eq!(open(&dir).last_match().unwrap(), None);
    }

    #[test]
    fn save_last_match_overwrites_the_single_row() {
        let (_tmp, dir) = open_tmp();
        let mut store = open(&dir);
        let id = store.add(entry("Show")).unwrap().id;
        store.save_last_match(&proposed("first.mkv")).unwrap();
        let second = ProposedMatch {
            link: Link::Exact(id),
            ..proposed("second.mkv")
        };
        store.save_last_match(&second).unwrap();
        assert_eq!(store.last_match().unwrap(), Some(second));
        let rows: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM last_match", [], |row| row.get(0))
            .unwrap();
        assert_eq!(rows, 1);
    }

    #[test]
    fn last_match_survives_reopen() {
        let (_tmp, dir) = open_tmp();
        let mut store = open(&dir);
        let m = proposed("Show - 03.mkv");
        store.save_last_match(&m).unwrap();
        drop(store);
        assert_eq!(open(&dir).last_match().unwrap(), Some(m));
    }

    #[test]
    fn last_match_with_unknown_entry_is_rejected_by_the_foreign_key() {
        let (_tmp, dir) = open_tmp();
        let mut store = open(&dir);
        let m = ProposedMatch {
            link: Link::Exact(EntryId(999)),
            ..proposed("Show - 03.mkv")
        };
        assert!(store.save_last_match(&m).is_err());
        assert_eq!(store.last_match().unwrap(), None);
    }

    #[test]
    fn garbage_confidence_surfaces_as_invalid_last_match() {
        let (_tmp, dir) = open_tmp();
        let mut store = open(&dir);
        store.save_last_match(&proposed("Show - 03.mkv")).unwrap();
        store.execute_raw("UPDATE last_match SET confidence = 'bogus'");
        assert!(matches!(
            store.last_match(),
            Err(StoreError::InvalidLastMatch {
                field: "confidence"
            })
        ));
    }

    #[test]
    fn exact_without_an_entry_surfaces_as_invalid_last_match() {
        let (_tmp, dir) = open_tmp();
        let mut store = open(&dir);
        store.save_last_match(&proposed("Show - 03.mkv")).unwrap();
        store.execute_raw("UPDATE last_match SET entry_id = NULL, confidence = 'exact'");
        assert!(matches!(
            store.last_match(),
            Err(StoreError::InvalidLastMatch { field: "link" })
        ));
    }

    #[test]
    fn unmatched_with_an_entry_surfaces_as_invalid_last_match() {
        let (_tmp, dir) = open_tmp();
        let mut store = open(&dir);
        let id = store.add(entry("Show")).unwrap().id;
        store.save_last_match(&proposed("Show - 03.mkv")).unwrap();
        store.execute_raw(&format!(
            "UPDATE last_match SET entry_id = {id}, confidence = 'unmatched'"
        ));
        assert!(matches!(
            store.last_match(),
            Err(StoreError::InvalidLastMatch { field: "link" })
        ));
    }
}
