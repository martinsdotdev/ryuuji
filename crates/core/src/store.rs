use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use std::{fmt, fs, io};

use rusqlite::ErrorCode;
use rusqlite::{Connection, OptionalExtension, Row, ToSql, params};
use rusqlite_migration::{M, Migrations};
use tracing::{debug, info, info_span, warn};

use crate::{DataDir, EntryId, LibraryEntry, NewEntry, WatchStatus};

const SCHEMA_V1: &str = "\
CREATE TABLE entries (
    id INTEGER PRIMARY KEY,
    title TEXT NOT NULL,
    status TEXT NOT NULL,
    progress INTEGER NOT NULL DEFAULT 0,
    total INTEGER,
    updated_at INTEGER NOT NULL
)";

const SELECT_ENTRY: &str = "SELECT id, title, status, progress, total FROM entries";

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
            | StoreError::InvalidRow { .. } => false,
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
            "INSERT INTO entries (title, status, progress, total, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                entry.title,
                entry.status.tag(),
                entry.progress,
                entry.total,
                unix_now()
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
        let changed = tx
            .execute(
                &format!("UPDATE entries SET {column} = ?1, updated_at = ?2 WHERE id = ?3"),
                params![value, unix_now(), id.0],
            )
            .map_err(query_failed)?;
        if changed == 0 {
            return Err(StoreError::NotFound { id });
        }
        let stored = fetch(&tx, id)?;
        tx.commit().map_err(query_failed)?;
        Ok(stored)
    }

    /// Reads `PRAGMA user_version`; touches nothing on disk.
    pub fn schema_version(&self) -> Result<SchemaVersion, StoreError> {
        self.conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .map(SchemaVersion)
            .map_err(query_failed)
    }

    #[cfg(test)]
    fn execute_raw(&self, sql: &str) {
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
    Migrations::from_slice(&[M::up(SCHEMA_V1)])
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

/// A row as SQLite hands it over, before the domain checks in `parse`.
struct RawEntry {
    id: i64,
    title: String,
    status: String,
    progress: i64,
    total: Option<i64>,
}

impl RawEntry {
    fn read(row: &Row<'_>) -> rusqlite::Result<RawEntry> {
        Ok(RawEntry {
            id: row.get(0)?,
            title: row.get(1)?,
            status: row.get(2)?,
            progress: row.get(3)?,
            total: row.get(4)?,
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
        })
    }
}

fn query_failed(err: rusqlite::Error) -> StoreError {
    StoreError::Query(err.into())
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| i64::try_from(elapsed.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
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
        }
    }

    fn schema_version(store: &Store) -> SchemaVersion {
        store.schema_version().unwrap()
    }

    #[test]
    fn open_creates_the_database_at_schema_v1_without_recovery() {
        let (_tmp, dir) = open_tmp();
        let store = open(&dir);
        assert!(dir.root().join("library.sqlite").is_file());
        assert_eq!(schema_version(&store), SchemaVersion(1));
        drop(store);
        assert_eq!(schema_version(&open(&dir)), SchemaVersion(1));
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
        assert_eq!(schema_version(&store), SchemaVersion(1));
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
}
