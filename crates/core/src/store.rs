use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, Row, ToSql, params};
use rusqlite_migration::{M, Migrations};
use tracing::{debug, info, info_span};

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
    #[error("no library entry with id {id}")]
    NotFound { id: EntryId },
    #[error("library entry {id} has an invalid {field}")]
    InvalidRow { id: EntryId, field: &'static str },
}

/// The on-disk library. Every mutation runs in one transaction and returns
/// the row as stored.
pub struct Store {
    conn: Connection,
}

impl Store {
    pub fn open(dir: &DataDir) -> Result<Store, StoreError> {
        let path = dir.library_db();
        let _span = info_span!("store.open", path = %path.display()).entered();
        let mut conn = Connection::open(&path).map_err(|err| StoreError::Open {
            path: path.clone(),
            source: err.into(),
        })?;
        Migrations::from_slice(&[M::up(SCHEMA_V1)])
            .to_latest(&mut conn)
            .map_err(|err| StoreError::Migrate(err.into()))?;
        conn.pragma_update(None, "synchronous", "FULL")
            .map_err(query_failed)?;
        info!("library opened");
        Ok(Store { conn })
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

    #[cfg(test)]
    fn execute_raw(&self, sql: &str) {
        self.conn.execute(sql, []).unwrap();
    }
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

    fn entry(title: &str) -> NewEntry {
        NewEntry {
            title: title.into(),
            status: WatchStatus::Watching,
            progress: 0,
            total: None,
        }
    }

    fn user_version(store: &Store) -> i64 {
        store
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap()
    }

    #[test]
    fn open_creates_the_database_at_schema_v1() {
        let (_tmp, dir) = open_tmp();
        let store = Store::open(&dir).unwrap();
        assert!(dir.root().join("library.sqlite").is_file());
        assert_eq!(user_version(&store), 1);
        drop(store);
        assert_eq!(user_version(&Store::open(&dir).unwrap()), 1);
    }

    #[test]
    fn add_returns_stored_rows_with_increasing_ids() {
        let (_tmp, dir) = open_tmp();
        let mut store = Store::open(&dir).unwrap();
        let first = store.add(entry("First")).unwrap();
        let second = store.add(entry("Second")).unwrap();
        assert_eq!(first.title, "First");
        assert!(second.id > first.id);
        assert_eq!(store.entries().unwrap(), vec![first, second]);
    }

    #[test]
    fn mutations_persist_across_reopen() {
        let (_tmp, dir) = open_tmp();
        let mut store = Store::open(&dir).unwrap();
        let id = store.add(entry("Show")).unwrap().id;
        let after_progress = store.set_progress(id, 7).unwrap();
        assert_eq!(after_progress.progress, 7);
        let after_status = store.set_status(id, WatchStatus::OnHold).unwrap();
        assert_eq!(after_status.status, WatchStatus::OnHold);
        drop(store);

        let reopened = Store::open(&dir).unwrap();
        assert_eq!(reopened.entries().unwrap(), vec![after_status]);
    }

    #[test]
    fn set_progress_on_unknown_id_is_not_found_and_changes_nothing() {
        let (_tmp, dir) = open_tmp();
        let mut store = Store::open(&dir).unwrap();
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
        let store = Store::open(&dir).unwrap();
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
