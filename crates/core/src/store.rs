use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use std::{fmt, fs, io};

use rusqlite::ErrorCode;
use rusqlite::{Connection, OptionalExtension, Row, ToSql, params};
use rusqlite_migration::Migrations;
use tracing::{debug, info, info_span, warn};

use crate::{
    Confidence, DataDir, Decline, EntryId, HistoryId, HistoryPage, LibraryEntry, Link, NewEntry,
    NewRecording, ProposedMatch, Recorded, Watch, WatchOutcome, WatchStatus,
};

/// The schema, compiled in from `migrations/`, one numbered directory per
/// version.
static MIGRATIONS: include_dir::Dir<'static> =
    include_dir::include_dir!("$CARGO_MANIFEST_DIR/migrations");

const SELECT_ENTRY: &str = "SELECT id, title, status, progress, total, rewatching FROM entries";

const SELECT_WATCH: &str = "SELECT id, entry_id, episode, episode_end, progress_before, progress, \
                            raw_title, player, at, undone_at, parsed_title, confidence, reason, \
                            recorded_at, added_at, kind FROM history";

/// The `kind` of a row the set columns cannot tell apart. Every other row
/// leaves it null and is read from its columns, as rows written before the
/// column existed are.
const IGNORED_KIND: &str = "ignored";

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
    #[error("history row {id} has an invalid {field}")]
    InvalidHistoryRow { id: HistoryId, field: &'static str },
    #[error("history row {id} is missing, not a recording, or already undone")]
    NothingToUndo { id: HistoryId },
    /// The entry's progress is no longer what the recording wrote, so
    /// putting `progress_before` back would erase whatever moved it since.
    #[error("the entry has moved on since history row {id} was recorded")]
    MovedOn { id: HistoryId },
    /// A write aimed at a watch's own row found it missing or already
    /// holding a recording, which nothing may overwrite.
    #[error("history row {id} is missing or has already recorded")]
    WatchClosed { id: HistoryId },
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
            | StoreError::InvalidHistoryRow { .. }
            | StoreError::NothingToUndo { .. }
            | StoreError::MovedOn { .. }
            | StoreError::WatchClosed { .. } => false,
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

/// An entry and the watch that last moved its progress, both as stored
/// after the same transaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Recording {
    pub entry: LibraryEntry,
    pub watch: Watch,
}

/// An entry added from a watch and that watch's row carrying the mark, both
/// as stored after the same transaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Added {
    pub entry: LibraryEntry,
    pub watch: Watch,
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
        let stored = insert_entry(&tx, entry)?;
        let id = stored.id;
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
    pub fn record(&mut self, recording: NewRecording) -> Result<Recording, StoreError> {
        let _span = info_span!(
            "store.record",
            entry = %recording.entry,
            episode = *recording.episode.end()
        )
        .entered();
        let tx = self.conn.transaction().map_err(query_failed)?;
        let before = fetch(&tx, recording.entry)?;
        let entry = set_column(&tx, recording.entry, "progress", *recording.episode.end())?;
        let columns = params![
            recording.entry.0,
            recording.episode.start(),
            recording.episode.end(),
            before.progress,
            entry.progress,
            recording.raw_title,
            recording.player,
            unix_now(),
            recording.parsed_title,
            Confidence::Exact.tag(),
        ];
        let id = match recording.watch {
            None => {
                tx.execute(
                    "INSERT INTO history (entry_id, episode, episode_end, progress_before, \
                     progress, raw_title, player, at, parsed_title, confidence, recorded_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?8)",
                    columns,
                )
                .map_err(query_failed)?;
                HistoryId(tx.last_insert_rowid())
            }
            // The row keeps the `at` it was opened with; the reason goes,
            // since the recording is what the watch came to.
            Some(id) => {
                let changed = tx
                    .execute(
                        &format!(
                            "UPDATE history SET entry_id = ?1, episode = ?2, episode_end = ?3, \
                             progress_before = ?4, progress = ?5, raw_title = ?6, player = ?7, \
                             recorded_at = ?8, parsed_title = ?9, confidence = ?10, \
                             reason = NULL WHERE id = {} AND progress IS NULL",
                            id.0
                        ),
                        columns,
                    )
                    .map_err(query_failed)?;
                if changed == 0 {
                    return Err(StoreError::WatchClosed { id });
                }
                id
            }
        };
        let watch = fetch_watch(&tx, id)?;
        tx.commit().map_err(query_failed)?;
        info!(watch = %id, progress = entry.progress, "progress recorded");
        Ok(Recording { entry, watch })
    }

    /// Adds the show a proposal names and marks the watch that asked for it,
    /// in one transaction, so neither exists without the other. The watch's
    /// own row takes the mark while it has not recorded and drops its
    /// reason, which was about a match that now exists; a watch with no row
    /// yet gets one.
    pub fn add_to_library(
        &mut self,
        entry: NewEntry,
        watch: Option<HistoryId>,
        proposal: &ProposedMatch,
    ) -> Result<Added, StoreError> {
        let _span =
            info_span!("store.add_to_library", title = %entry.title, watch = ?watch).entered();
        let tx = self.conn.transaction().map_err(query_failed)?;
        let entry = insert_entry(&tx, entry)?;
        let now = unix_now();
        let id = match watch {
            None => {
                tx.execute(
                    "INSERT INTO history (entry_id, episode, episode_end, raw_title, player, at, \
                     parsed_title, confidence, added_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?6)",
                    params![
                        entry.id.0,
                        proposal.episode.as_ref().map(|r| i64::from(*r.start())),
                        proposal.episode.as_ref().map(|r| i64::from(*r.end())),
                        proposal.raw_title,
                        proposal.player,
                        now,
                        proposal.parsed_title,
                        Confidence::Exact.tag(),
                    ],
                )
                .map_err(query_failed)?;
                HistoryId(tx.last_insert_rowid())
            }
            Some(id) => {
                let changed = tx
                    .execute(
                        "UPDATE history SET entry_id = ?1, confidence = ?2, reason = NULL, \
                         added_at = ?3 WHERE id = ?4 AND progress IS NULL",
                        params![entry.id.0, Confidence::Exact.tag(), now, id.0],
                    )
                    .map_err(query_failed)?;
                if changed == 0 {
                    return Err(StoreError::WatchClosed { id });
                }
                id
            }
        };
        let watch = fetch_watch(&tx, id)?;
        tx.commit().map_err(query_failed)?;
        info!(entry = %entry.id, watch = %id, "entry added from a watch");
        Ok(Added { entry, watch })
    }

    /// Writes why a watch past its threshold recorded nothing: a new row, or
    /// the watch's own row rewritten while it has not recorded. The link is
    /// rewritten with the reason, since a library edit can change the match
    /// between one refusal and the next.
    pub fn decline(
        &mut self,
        watch: Option<HistoryId>,
        proposal: &ProposedMatch,
        decline: Decline,
    ) -> Result<Watch, StoreError> {
        let _span = info_span!("store.decline", watch = ?watch, reason = decline.tag()).entered();
        let tx = self.conn.transaction().map_err(query_failed)?;
        let entry = proposal.link.entry().map(EntryId::as_i64);
        let confidence = proposal.link.confidence().tag();
        let id = match watch {
            None => {
                tx.execute(
                    "INSERT INTO history (entry_id, episode, episode_end, raw_title, player, at, \
                     parsed_title, confidence, reason) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                    params![
                        entry,
                        proposal.episode.as_ref().map(|r| i64::from(*r.start())),
                        proposal.episode.as_ref().map(|r| i64::from(*r.end())),
                        proposal.raw_title,
                        proposal.player,
                        unix_now(),
                        proposal.parsed_title,
                        confidence,
                        decline.tag(),
                    ],
                )
                .map_err(query_failed)?;
                HistoryId(tx.last_insert_rowid())
            }
            Some(id) => {
                let changed = tx
                    .execute(
                        "UPDATE history SET entry_id = ?1, confidence = ?2, reason = ?3 \
                         WHERE id = ?4 AND progress IS NULL",
                        params![entry, confidence, decline.tag(), id.0],
                    )
                    .map_err(query_failed)?;
                if changed == 0 {
                    return Err(StoreError::WatchClosed { id });
                }
                id
            }
        };
        let watch = fetch_watch(&tx, id)?;
        tx.commit().map_err(query_failed)?;
        info!(watch = %id, reason = decline.tag(), "decline logged");
        Ok(watch)
    }

    /// Marks the recording undone and puts its `progress_before` back on
    /// the entry, in one transaction. Restoring rather than decrementing is
    /// what takes a batch back to where it started. The row stays: nothing
    /// here deletes, and the mark is what stops a second undo. An entry
    /// whose progress has moved past the recording, by a later one or by
    /// hand, is refused, since restoring would erase that move.
    pub fn undo(&mut self, id: HistoryId) -> Result<Recording, StoreError> {
        let _span = info_span!("store.undo", watch = %id).entered();
        let tx = self.conn.transaction().map_err(query_failed)?;
        let watch = find_watch(&tx, id)?.ok_or(StoreError::NothingToUndo { id })?;
        let (entry_id, recorded) = match (watch.outcome, watch.link.entry()) {
            (WatchOutcome::Recorded(recorded), Some(entry)) if recorded.undone_at.is_none() => {
                (entry, recorded)
            }
            _ => return Err(StoreError::NothingToUndo { id }),
        };
        if fetch(&tx, entry_id)?.progress != recorded.progress {
            return Err(StoreError::MovedOn { id });
        }
        tx.execute(
            "UPDATE history SET undone_at = ?1 WHERE id = ?2",
            params![unix_now(), id.0],
        )
        .map_err(query_failed)?;
        let watch = fetch_watch(&tx, id)?;
        let entry = set_column(&tx, entry_id, "progress", recorded.progress_before)?;
        tx.commit().map_err(query_failed)?;
        info!(entry = %entry.id, progress = entry.progress, "progress restored");
        Ok(Recording { entry, watch })
    }

    /// Up to `limit` watches, newest first, for one entry or all of them.
    /// One row past the limit is read to learn whether older ones exist.
    pub fn history(&self, entry: Option<EntryId>, limit: usize) -> Result<HistoryPage, StoreError> {
        let _span = info_span!("store.history", entry = ?entry, limit).entered();
        let wanted = i64::try_from(limit).unwrap_or(i64::MAX).saturating_add(1);
        // One statement per case, so the entry filter can use the index.
        let (filter, args) = match entry {
            Some(id) => (
                "WHERE entry_id = ?1 ORDER BY id DESC LIMIT ?2",
                vec![id.0, wanted],
            ),
            None => ("ORDER BY id DESC LIMIT ?1", vec![wanted]),
        };
        let mut stmt = self
            .conn
            .prepare(&format!("{SELECT_WATCH} {filter}"))
            .map_err(query_failed)?;
        let mut watches = stmt
            .query_map(rusqlite::params_from_iter(args), RawWatch::read)
            .map_err(query_failed)?
            .map(|row| row.map_err(query_failed)?.parse())
            .collect::<Result<Vec<_>, _>>()?;
        let more = watches.len() > limit;
        watches.truncate(limit);
        Ok(HistoryPage { watches, more })
    }

    /// How many episodes the recordings made at or after `since` wrote and
    /// still stand: a batch counts every episode it carried, and an undone
    /// recording counts none. Read from the table, so the count does not
    /// depend on how much of the history a page has loaded.
    pub fn episodes_recorded_since(&self, since: SystemTime) -> Result<u32, StoreError> {
        let _span = info_span!("store.episodes_recorded_since").entered();
        let count: i64 = self
            .conn
            .query_row(
                "SELECT COALESCE(SUM(episode_end - episode + 1), 0) FROM history \
                 WHERE progress IS NOT NULL AND undone_at IS NULL AND recorded_at >= ?1",
                [unix_secs(since)],
                |row| row.get(0),
            )
            .map_err(query_failed)?;
        Ok(u32::try_from(count).unwrap_or(u32::MAX))
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

    /// Remembers that a title names `entry`, so matching settles on it from
    /// now on. `needle` is the folded form matching compares, and
    /// `parsed_title` what was read; writing the same needle again replaces
    /// the row, which is how a wrong confirmation is corrected.
    pub fn remember_title(
        &mut self,
        needle: &str,
        parsed_title: &str,
        entry: EntryId,
    ) -> Result<(), StoreError> {
        let _span = info_span!("store.remember_title", entry = %entry).entered();
        self.conn
            .execute(
                "INSERT OR REPLACE INTO aliases (needle, parsed_title, entry_id, at) \
                 VALUES (?1, ?2, ?3, ?4)",
                params![needle, parsed_title, entry.0, unix_now()],
            )
            .map_err(query_failed)?;
        info!("title remembered");
        Ok(())
    }

    /// Every remembered title with the entry it names.
    pub fn aliases(&self) -> Result<Vec<(String, EntryId)>, StoreError> {
        let _span = info_span!("store.aliases").entered();
        let mut stmt = self
            .conn
            .prepare("SELECT needle, entry_id FROM aliases ORDER BY needle")
            .map_err(query_failed)?;
        let rows = stmt
            .query_map([], |row| Ok((row.get(0)?, EntryId(row.get(1)?))))
            .map_err(query_failed)?;
        let aliases = rows
            .map(|row| row.map_err(query_failed))
            .collect::<Result<Vec<_>, _>>()?;
        debug!(count = aliases.len(), "aliases loaded");
        Ok(aliases)
    }

    /// Remembers not to track a file, and marks the watch that asked for it,
    /// in one transaction so neither exists without the other. The watch's
    /// own row takes the mark while it has not recorded and drops its
    /// reason, which was about a match no longer being looked for; a watch
    /// with no row yet gets one.
    pub fn ignore_file(
        &mut self,
        watch: Option<HistoryId>,
        proposal: &ProposedMatch,
    ) -> Result<Watch, StoreError> {
        let _span = info_span!("store.ignore_file", watch = ?watch).entered();
        let tx = self.conn.transaction().map_err(query_failed)?;
        let now = unix_now();
        // Ignoring the same title again renews it, which is what saying so
        // after a stop means.
        tx.execute(
            "INSERT OR REPLACE INTO ignored (raw_title, at, stopped_at) VALUES (?1, ?2, NULL)",
            params![proposal.raw_title, now],
        )
        .map_err(query_failed)?;
        let entry = proposal.link.entry().map(EntryId::as_i64);
        let confidence = proposal.link.confidence().tag();
        let id = match watch {
            None => {
                tx.execute(
                    "INSERT INTO history (entry_id, episode, episode_end, raw_title, player, at, \
                     parsed_title, confidence, kind) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                    params![
                        entry,
                        proposal.episode.as_ref().map(|r| i64::from(*r.start())),
                        proposal.episode.as_ref().map(|r| i64::from(*r.end())),
                        proposal.raw_title,
                        proposal.player,
                        now,
                        proposal.parsed_title,
                        confidence,
                        IGNORED_KIND,
                    ],
                )
                .map_err(query_failed)?;
                HistoryId(tx.last_insert_rowid())
            }
            Some(id) => {
                let changed = tx
                    .execute(
                        "UPDATE history SET entry_id = ?1, confidence = ?2, reason = NULL, \
                         kind = ?3 WHERE id = ?4 AND progress IS NULL",
                        params![entry, confidence, IGNORED_KIND, id.0],
                    )
                    .map_err(query_failed)?;
                if changed == 0 {
                    return Err(StoreError::WatchClosed { id });
                }
                id
            }
        };
        let watch = fetch_watch(&tx, id)?;
        tx.commit().map_err(query_failed)?;
        info!(watch = %id, "file ignored");
        Ok(watch)
    }

    /// Stops ignoring a file. The row is marked rather than removed, so the
    /// history rows that say the file was ignored still have what they
    /// refer to, and ignoring it again is a fresh row.
    pub fn stop_ignoring(&mut self, raw_title: &str) -> Result<(), StoreError> {
        let _span = info_span!("store.stop_ignoring").entered();
        self.conn
            .execute(
                "UPDATE ignored SET stopped_at = ?1 WHERE raw_title = ?2 AND stopped_at IS NULL",
                params![unix_now(), raw_title],
            )
            .map_err(query_failed)?;
        info!("no longer ignored");
        Ok(())
    }

    /// Every file still being ignored, by the raw title a player reports.
    pub fn ignored(&self) -> Result<Vec<String>, StoreError> {
        let _span = info_span!("store.ignored").entered();
        let mut stmt = self
            .conn
            .prepare("SELECT raw_title FROM ignored WHERE stopped_at IS NULL ORDER BY raw_title")
            .map_err(query_failed)?;
        let rows = stmt.query_map([], |row| row.get(0)).map_err(query_failed)?;
        let ignored = rows
            .map(|row| row.map_err(query_failed))
            .collect::<Result<Vec<String>, _>>()?;
        debug!(count = ignored.len(), "ignored files loaded");
        Ok(ignored)
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

/// Inserts one entry on the caller's connection, so a transaction can pair
/// it with another write, and reads it back as stored.
fn insert_entry(conn: &Connection, entry: NewEntry) -> Result<LibraryEntry, StoreError> {
    conn.execute(
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
    fetch(conn, EntryId(conn.last_insert_rowid()))
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

fn fetch_watch(conn: &Connection, id: HistoryId) -> Result<Watch, StoreError> {
    find_watch(conn, id)?.ok_or(StoreError::InvalidHistoryRow { id, field: "id" })
}

fn find_watch(conn: &Connection, id: HistoryId) -> Result<Option<Watch>, StoreError> {
    conn.query_row(
        &format!("{SELECT_WATCH} WHERE id = ?1"),
        [id.0],
        RawWatch::read,
    )
    .optional()
    .map_err(query_failed)?
    .map(RawWatch::parse)
    .transpose()
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

/// A history row as SQLite hands it over, before the domain checks in
/// `parse`.
struct RawWatch {
    id: i64,
    entry_id: Option<i64>,
    episode: Option<i64>,
    episode_end: Option<i64>,
    progress_before: Option<i64>,
    progress: Option<i64>,
    raw_title: Option<String>,
    player: Option<String>,
    at: i64,
    undone_at: Option<i64>,
    parsed_title: Option<String>,
    confidence: String,
    reason: Option<String>,
    recorded_at: Option<i64>,
    added_at: Option<i64>,
    kind: Option<String>,
}

impl RawWatch {
    // Positional, so `SELECT_WATCH` only ever appends a column.
    fn read(row: &Row<'_>) -> rusqlite::Result<RawWatch> {
        Ok(RawWatch {
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
            parsed_title: row.get(10)?,
            confidence: row.get(11)?,
            reason: row.get(12)?,
            recorded_at: row.get(13)?,
            added_at: row.get(14)?,
            kind: row.get(15)?,
        })
    }

    /// The outcome is `kind` when it names one, else it is read from which
    /// columns are set: the progress pair means a recording, else a reason
    /// means a decline, else the added mark alone.
    fn parse(self) -> Result<Watch, StoreError> {
        let id = HistoryId(self.id);
        let invalid = |field| StoreError::InvalidHistoryRow { id, field };
        let time = |secs: i64, field| unix_time(secs).ok_or_else(|| invalid(field));
        let count = |value: i64, field| u32::try_from(value).map_err(|_| invalid(field));
        let confidence =
            Confidence::from_tag(&self.confidence).ok_or_else(|| invalid("confidence"))?;
        let link = Link::from_columns(self.entry_id.map(EntryId), confidence)
            .ok_or_else(|| invalid("link"))?;
        let episode =
            episode_range(self.episode, self.episode_end).map_err(|()| invalid("episode"))?;
        let outcome = match self.kind.as_deref() {
            // An ignore is nothing being done, so a row claiming one
            // alongside a write or a refusal is contradictory.
            Some(IGNORED_KIND) => {
                if self.progress.is_some() || self.reason.is_some() {
                    return Err(invalid("kind"));
                }
                WatchOutcome::Ignored
            }
            Some(_) => return Err(invalid("kind")),
            None => match (self.progress_before, self.progress, self.recorded_at) {
                (Some(before), Some(progress), Some(recorded_at)) => {
                    if !matches!(link, Link::Exact(_)) {
                        return Err(invalid("link"));
                    }
                    if episode.is_none() {
                        return Err(invalid("episode"));
                    }
                    WatchOutcome::Recorded(Recorded {
                        progress_before: count(before, "progress_before")?,
                        progress: count(progress, "progress")?,
                        at: time(recorded_at, "recorded_at")?,
                        undone_at: self.undone_at.map(|at| time(at, "undone_at")).transpose()?,
                    })
                }
                (None, None, None) if self.undone_at.is_some() => return Err(invalid("undone_at")),
                (None, None, None) => match (&self.reason, self.added_at) {
                    (Some(reason), _) => WatchOutcome::Declined(
                        Decline::from_tag(reason).ok_or_else(|| invalid("reason"))?,
                    ),
                    (None, Some(_)) => WatchOutcome::Added,
                    (None, None) => return Err(invalid("outcome")),
                },
                _ => return Err(invalid("progress")),
            },
        };
        Ok(Watch {
            id,
            at: time(self.at, "at")?,
            raw_title: self.raw_title.ok_or_else(|| invalid("raw_title"))?,
            parsed_title: self.parsed_title.unwrap_or_default(),
            player: self.player.ok_or_else(|| invalid("player"))?,
            episode,
            link,
            outcome,
            added_at: self.added_at.map(|at| time(at, "added_at")).transpose()?,
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
    fn open_creates_the_database_at_schema_v8_without_recovery() {
        let (_tmp, dir) = open_tmp();
        let store = open(&dir);
        assert!(dir.root().join("library.sqlite").is_file());
        assert_eq!(schema_version(&store), SchemaVersion(8));
        drop(store);
        assert_eq!(schema_version(&open(&dir)), SchemaVersion(8));
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
        assert_eq!(schema_version(&store), SchemaVersion(8));
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
    fn v1_library_migrates_to_v7_and_keeps_entries() {
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
        assert_eq!(schema_version(&store), SchemaVersion(8));
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
        assert_eq!(schema_version(&store), SchemaVersion(8));
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
        assert_eq!(schema_version(&store), SchemaVersion(8));
        assert_eq!(store.last_match().unwrap(), Some(proposed("Show - 03.mkv")));
    }

    #[test]
    fn v4_library_migrates_to_v7_with_an_empty_history() {
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
        assert_eq!(schema_version(&store), SchemaVersion(8));
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
        assert_eq!(watches(&store), vec![]);
    }

    #[test]
    fn v5_watch_events_become_recorded_history_rows() {
        let (_tmp, dir) = open_tmp();
        let conn = Connection::open(dir.library_db()).unwrap();
        for sql in [
            include_str!("../migrations/01-entries/up.sql"),
            include_str!("../migrations/02-last_match/up.sql"),
            include_str!("../migrations/03-drop_outcome/up.sql"),
            include_str!("../migrations/04-episode_range_and_rewatching/up.sql"),
            include_str!("../migrations/05-watch_events/up.sql"),
        ] {
            conn.execute_batch(sql).unwrap();
        }
        conn.execute_batch(
            "INSERT INTO entries (id, title, status, progress, total, updated_at) \
             VALUES (7, 'Show', 'watching', 1, 12, 0); \
             INSERT INTO watch_events (id, entry_id, episode, episode_end, progress_before, \
             progress, raw_title, player, at, undone_at) VALUES \
             (4, 7, 1, 1, 0, 1, 'Show - 01.mkv', 'mpv', 1700000000, NULL), \
             (9, 7, 2, 3, 1, 3, 'Show - 02-03.mkv', 'vlc', 1700000100, 1700000200);",
        )
        .unwrap();
        conn.pragma_update(None, "user_version", 5).unwrap();
        drop(conn);

        let store = open(&dir);
        assert_eq!(schema_version(&store), SchemaVersion(8));
        let at = |secs: u64| UNIX_EPOCH + Duration::from_secs(secs);
        let copied = |id: i64,
                      episode: RangeInclusive<u32>,
                      progress_before: u32,
                      progress: u32,
                      raw_title: &str,
                      player: &str,
                      secs: u64,
                      undone_at: Option<SystemTime>| Watch {
            id: HistoryId(id),
            at: at(secs),
            raw_title: raw_title.into(),
            parsed_title: String::new(),
            player: player.into(),
            episode: Some(episode),
            link: Link::Exact(EntryId(7)),
            outcome: WatchOutcome::Recorded(Recorded {
                progress_before,
                progress,
                at: at(secs),
                undone_at,
            }),
            added_at: None,
        };
        assert_eq!(
            store.history(None, 10).unwrap(),
            HistoryPage {
                watches: vec![
                    copied(
                        9,
                        2..=3,
                        1,
                        3,
                        "Show - 02-03.mkv",
                        "vlc",
                        1_700_000_100,
                        Some(at(1_700_000_200)),
                    ),
                    copied(4, 1..=1, 0, 1, "Show - 01.mkv", "mpv", 1_700_000_000, None),
                ],
                more: false,
            }
        );
        let mut stmt = store
            .conn
            .prepare("SELECT name FROM sqlite_master")
            .unwrap();
        let names: Vec<String> = stmt
            .query_map([], |row| row.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert!(names.iter().any(|name| name == "history_by_entry"));
        assert!(!names.iter().any(|name| name.starts_with("watch_events")));
    }

    #[test]
    fn v6_library_migrates_to_v8_with_nothing_remembered_or_ignored() {
        let (_tmp, dir) = open_tmp();
        let conn = Connection::open(dir.library_db()).unwrap();
        for sql in [
            include_str!("../migrations/01-entries/up.sql"),
            include_str!("../migrations/02-last_match/up.sql"),
            include_str!("../migrations/03-drop_outcome/up.sql"),
            include_str!("../migrations/04-episode_range_and_rewatching/up.sql"),
            include_str!("../migrations/05-watch_events/up.sql"),
            include_str!("../migrations/06-history/up.sql"),
        ] {
            conn.execute_batch(sql).unwrap();
        }
        conn.execute(
            "INSERT INTO entries (id, title, status, progress, total, updated_at) \
             VALUES (7, 'Show', 'watching', 3, 12, 0)",
            [],
        )
        .unwrap();
        conn.pragma_update(None, "user_version", 6).unwrap();
        drop(conn);

        let store = open(&dir);
        assert_eq!(schema_version(&store), SchemaVersion(8));
        let entries = store.entries().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, EntryId(7));
        assert_eq!(store.aliases().unwrap(), vec![]);
        assert!(store.ignored().unwrap().is_empty());
    }

    fn watching(id: EntryId, episode: RangeInclusive<u32>) -> NewRecording {
        NewRecording {
            watch: None,
            entry: id,
            episode,
            raw_title: "Show - 03.mkv".into(),
            parsed_title: "Show".into(),
            player: "mpv".into(),
        }
    }

    fn watches(store: &Store) -> Vec<Watch> {
        store.history(None, 100).unwrap().watches
    }

    fn recorded(watch: &Watch) -> Recorded {
        match watch.outcome {
            WatchOutcome::Recorded(recorded) => recorded,
            other => panic!("not a recording: {other:?}"),
        }
    }

    #[test]
    fn decline_writes_a_row_and_rewrites_it_while_unrecorded() {
        let (_tmp, dir) = open_tmp();
        let mut store = open(&dir);
        let id = store.add(entry("Show")).unwrap().id;
        let first = store
            .decline(None, &proposed("Show - 03.mkv"), Decline::NotExact)
            .unwrap();
        assert_eq!(first.outcome, WatchOutcome::Declined(Decline::NotExact));
        assert_eq!(first.link, Link::Unmatched);
        assert_eq!(first.episode, Some(3..=3));
        assert_eq!(first.raw_title, "Show - 03.mkv");
        assert_eq!(first.parsed_title, "Show");
        assert_eq!(first.player, "mpv");
        assert_eq!(first.added_at, None);

        let linked = ProposedMatch {
            link: Link::Exact(id),
            ..proposed("Show - 03.mkv")
        };
        let second = store
            .decline(Some(first.id), &linked, Decline::NotNext)
            .unwrap();
        assert_eq!(
            second,
            Watch {
                link: Link::Exact(id),
                outcome: WatchOutcome::Declined(Decline::NotNext),
                ..first
            }
        );
        assert_eq!(watches(&store), vec![second]);
    }

    #[test]
    fn ignoring_a_file_writes_a_row_and_lists_it_until_stopped() {
        let (_tmp, dir) = open_tmp();
        let mut store = open(&dir);
        let watch = store.ignore_file(None, &proposed("Show - 03.mkv")).unwrap();
        assert_eq!(watch.outcome, WatchOutcome::Ignored);
        assert_eq!(watch.raw_title, "Show - 03.mkv");
        assert_eq!(watch.episode, Some(3..=3));
        assert_eq!(store.ignored().unwrap(), vec!["Show - 03.mkv".to_owned()]);

        store.stop_ignoring("Show - 03.mkv").unwrap();
        assert!(store.ignored().unwrap().is_empty());
        // Nothing here deletes: the row saying it was ignored stays put.
        assert_eq!(watches(&store), vec![watch]);
    }

    #[test]
    fn ignoring_fills_the_declined_row_rather_than_opening_a_second() {
        let (_tmp, dir) = open_tmp();
        let mut store = open(&dir);
        let declined = store
            .decline(None, &proposed("Show - 03.mkv"), Decline::NotExact)
            .unwrap();
        let ignored = store
            .ignore_file(Some(declined.id), &proposed("Show - 03.mkv"))
            .unwrap();
        assert_eq!(ignored.id, declined.id);
        assert_eq!(ignored.outcome, WatchOutcome::Ignored);
        assert_eq!(watches(&store), vec![ignored]);
    }

    #[test]
    fn ignoring_a_file_again_after_stopping_starts_it_over() {
        let (_tmp, dir) = open_tmp();
        let mut store = open(&dir);
        store.ignore_file(None, &proposed("Show - 03.mkv")).unwrap();
        store.stop_ignoring("Show - 03.mkv").unwrap();
        assert!(store.ignored().unwrap().is_empty());
        store.ignore_file(None, &proposed("Show - 03.mkv")).unwrap();
        assert_eq!(store.ignored().unwrap(), vec!["Show - 03.mkv".to_owned()]);
    }

    #[test]
    fn an_ignored_file_survives_reopen() {
        let (_tmp, dir) = open_tmp();
        let mut store = open(&dir);
        store.ignore_file(None, &proposed("Show - 03.mkv")).unwrap();
        drop(store);
        assert_eq!(
            open(&dir).ignored().unwrap(),
            vec!["Show - 03.mkv".to_owned()]
        );
    }

    #[test]
    fn record_fills_a_declined_row_and_nothing_overwrites_it_after() {
        let (_tmp, dir) = open_tmp();
        let mut store = open(&dir);
        let id = store.add(entry("Show")).unwrap().id;
        let declined = store
            .decline(None, &proposed("Show - 03.mkv"), Decline::NotExact)
            .unwrap();

        let Recording { entry, watch } = store
            .record(NewRecording {
                watch: Some(declined.id),
                ..watching(id, 1..=1)
            })
            .unwrap();
        assert_eq!(entry.progress, 1);
        assert_eq!(watch.id, declined.id);
        assert_eq!(watch.at, declined.at);
        assert_eq!(watch.link, Link::Exact(id));
        assert_eq!(watch.episode, Some(1..=1));
        assert!(matches!(
            watch.outcome,
            WatchOutcome::Recorded(Recorded {
                progress_before: 0,
                progress: 1,
                undone_at: None,
                ..
            })
        ));

        assert!(matches!(
            store.decline(Some(watch.id), &proposed("Show - 03.mkv"), Decline::NotNext),
            Err(StoreError::WatchClosed { id: got }) if got == watch.id
        ));
        assert!(matches!(
            store.record(NewRecording {
                watch: Some(watch.id),
                ..watching(id, 2..=2)
            }),
            Err(StoreError::WatchClosed { id: got }) if got == watch.id
        ));
        assert_eq!(store.entries().unwrap(), vec![entry]);
        assert_eq!(watches(&store), vec![watch]);
    }

    #[test]
    fn undo_refuses_a_row_that_never_recorded() {
        let (_tmp, dir) = open_tmp();
        let mut store = open(&dir);
        let declined = store
            .decline(None, &proposed("Show - 03.mkv"), Decline::NotExact)
            .unwrap();
        assert!(matches!(
            store.undo(declined.id),
            Err(StoreError::NothingToUndo { id }) if id == declined.id
        ));
        assert_eq!(watches(&store), vec![declined]);
    }

    #[test]
    fn add_to_library_marks_the_watch_or_opens_a_row_for_it() {
        let (_tmp, dir) = open_tmp();
        let mut store = open(&dir);
        let declined = store
            .decline(None, &proposed("Show - 03.mkv"), Decline::NotExact)
            .unwrap();

        let Added { entry: show, watch } = store
            .add_to_library(entry("Show"), Some(declined.id), &proposed("Show - 03.mkv"))
            .unwrap();
        assert_eq!(store.entries().unwrap(), vec![show.clone()]);
        let added_at = watch.added_at.expect("the watch is marked");
        assert_eq!(
            watch,
            Watch {
                link: Link::Exact(show.id),
                outcome: WatchOutcome::Added,
                added_at: Some(added_at),
                ..declined
            }
        );

        let linked = ProposedMatch {
            link: Link::Exact(show.id),
            ..proposed("Show - 03.mkv")
        };
        let refused = store
            .decline(Some(watch.id), &linked, Decline::NotNext)
            .unwrap();
        assert_eq!(refused.outcome, WatchOutcome::Declined(Decline::NotNext));
        assert_eq!(refused.added_at, Some(added_at));

        let no_episode = ProposedMatch {
            episode: None,
            ..proposed("Other.mkv")
        };
        let fresh = store
            .add_to_library(entry("Other"), None, &no_episode)
            .unwrap();
        assert_eq!(fresh.watch.outcome, WatchOutcome::Added);
        assert_eq!(fresh.watch.episode, None);
        assert_eq!(fresh.watch.link, Link::Exact(fresh.entry.id));
        assert_eq!(fresh.watch.added_at, Some(fresh.watch.at));
        assert_eq!(watches(&store), vec![fresh.watch, refused]);
    }

    #[test]
    fn add_to_library_refuses_a_recorded_watch_and_adds_nothing() {
        let (_tmp, dir) = open_tmp();
        let mut store = open(&dir);
        let id = store.add(entry("Show")).unwrap().id;
        let written = store.record(watching(id, 1..=1)).unwrap();

        assert!(matches!(
            store.add_to_library(
                entry("Again"),
                Some(written.watch.id),
                &proposed("Show - 03.mkv")
            ),
            Err(StoreError::WatchClosed { id: got }) if got == written.watch.id
        ));
        assert_eq!(store.entries().unwrap(), vec![written.entry]);
        assert_eq!(watches(&store), vec![written.watch]);
    }

    #[test]
    fn episodes_recorded_since_counts_the_episodes_that_still_stand() {
        let (_tmp, dir) = open_tmp();
        let mut store = open(&dir);
        let a = store.add(entry("A")).unwrap().id;
        let b = store.add(entry("B")).unwrap().id;
        store.record(watching(a, 1..=1)).unwrap();
        let batch = store.record(watching(b, 1..=3)).unwrap();
        store
            .decline(None, &proposed("Show - 03.mkv"), Decline::NotExact)
            .unwrap();
        assert_eq!(store.episodes_recorded_since(UNIX_EPOCH).unwrap(), 4);

        store.undo(batch.watch.id).unwrap();
        assert_eq!(store.episodes_recorded_since(UNIX_EPOCH).unwrap(), 1);
        let hour_ahead = SystemTime::now() + Duration::from_secs(3_600);
        assert_eq!(store.episodes_recorded_since(hour_ahead).unwrap(), 0);

        store.execute_raw("UPDATE history SET recorded_at = 100 WHERE progress IS NOT NULL");
        let at = |secs| UNIX_EPOCH + Duration::from_secs(secs);
        assert_eq!(store.episodes_recorded_since(at(100)).unwrap(), 1);
        assert_eq!(store.episodes_recorded_since(at(101)).unwrap(), 0);
    }

    #[test]
    fn history_reads_newest_first_up_to_the_limit_and_filters_by_entry() {
        let (_tmp, dir) = open_tmp();
        let mut store = open(&dir);
        let a = store.add(entry("A")).unwrap().id;
        let b = store.add(entry("B")).unwrap().id;
        let a1 = store.record(watching(a, 1..=1)).unwrap().watch;
        let b1 = store.record(watching(b, 1..=1)).unwrap().watch;
        let a2 = store.record(watching(a, 2..=2)).unwrap().watch;
        let page = |watches: &[&Watch], more| HistoryPage {
            watches: watches.iter().map(|watch| (*watch).clone()).collect(),
            more,
        };
        assert_eq!(
            store.history(None, 3).unwrap(),
            page(&[&a2, &b1, &a1], false)
        );
        assert_eq!(store.history(None, 2).unwrap(), page(&[&a2, &b1], true));
        assert_eq!(store.history(Some(a), 1).unwrap(), page(&[&a2], true));
        assert_eq!(store.history(Some(b), 5).unwrap(), page(&[&b1], false));
        assert_eq!(store.history(None, 0).unwrap(), page(&[], true));
    }

    #[test]
    fn record_writes_progress_and_its_row_together() {
        let (_tmp, dir) = open_tmp();
        let mut store = open(&dir);
        let id = store.add(entry("Show")).unwrap().id;
        store.set_progress(id, 2).unwrap();

        let Recording { entry, watch } = store.record(watching(id, 3..=4)).unwrap();
        assert_eq!(entry.progress, 4);
        assert_eq!(watch.link, Link::Exact(id));
        assert_eq!(watch.episode, Some(3..=4));
        assert_eq!(watch.raw_title, "Show - 03.mkv");
        assert_eq!(watch.parsed_title, "Show");
        assert_eq!(watch.player, "mpv");
        assert!(watch.at >= UNIX_EPOCH + Duration::from_secs(1_700_000_000));
        assert_eq!(watch.added_at, None);
        assert_eq!(
            recorded(&watch),
            Recorded {
                progress_before: 2,
                progress: 4,
                at: watch.at,
                undone_at: None,
            }
        );

        let second = store.record(watching(id, 5..=5)).unwrap();
        assert!(second.watch.id > watch.id);
        assert_eq!(recorded(&second.watch).progress_before, 4);
        drop(store);

        let reopened = open(&dir);
        assert_eq!(reopened.entries().unwrap(), vec![second.entry]);
        assert_eq!(watches(&reopened), vec![second.watch, watch]);
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
        assert_eq!(watches(&store), vec![]);
    }

    #[test]
    fn undo_restores_progress_before_and_marks_the_row() {
        let (_tmp, dir) = open_tmp();
        let mut store = open(&dir);
        let id = store.add(entry("Show")).unwrap().id;
        store.set_progress(id, 2).unwrap();
        let written = store.record(watching(id, 3..=5)).unwrap();
        assert_eq!(written.entry.progress, 5);

        let Recording { entry, watch } = store.undo(written.watch.id).unwrap();
        assert_eq!(entry.progress, 2);
        let undone_at = recorded(&watch).undone_at;
        assert!(undone_at.is_some());
        assert_eq!(
            watch,
            Watch {
                outcome: WatchOutcome::Recorded(Recorded {
                    undone_at,
                    ..recorded(&written.watch)
                }),
                ..written.watch
            }
        );
        drop(store);

        let reopened = open(&dir);
        assert_eq!(reopened.entries().unwrap(), vec![entry]);
        assert_eq!(watches(&reopened), vec![watch]);
    }

    #[test]
    fn undo_refuses_an_undone_or_unknown_watch_and_changes_nothing() {
        let (_tmp, dir) = open_tmp();
        let mut store = open(&dir);
        let id = store.add(entry("Show")).unwrap().id;
        let watch = store.record(watching(id, 1..=1)).unwrap().watch.id;
        let undone = store.undo(watch).unwrap();
        let moved_on = store.set_progress(id, 4).unwrap();

        assert!(matches!(
            store.undo(watch),
            Err(StoreError::NothingToUndo { id }) if id == watch
        ));
        let unknown = HistoryId(watch.0 + 100);
        assert!(matches!(
            store.undo(unknown),
            Err(StoreError::NothingToUndo { id }) if id == unknown
        ));
        assert_eq!(store.entries().unwrap(), vec![moved_on]);
        assert_eq!(watches(&store), vec![undone.watch]);
    }

    #[test]
    fn undo_refuses_a_recording_the_entry_has_moved_past() {
        let (_tmp, dir) = open_tmp();
        let mut store = open(&dir);
        let id = store.add(entry("Show")).unwrap().id;
        let first = store.record(watching(id, 1..=1)).unwrap().watch;
        let second = store.record(watching(id, 2..=2)).unwrap();

        assert!(matches!(
            store.undo(first.id),
            Err(StoreError::MovedOn { id: got }) if got == first.id
        ));
        assert_eq!(store.entries().unwrap(), vec![second.entry.clone()]);
        assert_eq!(watches(&store), vec![second.watch.clone(), first.clone()]);

        let by_hand = store.set_progress(id, 7).unwrap();
        assert!(matches!(
            store.undo(second.watch.id),
            Err(StoreError::MovedOn { id: got }) if got == second.watch.id
        ));
        assert_eq!(store.entries().unwrap(), vec![by_hand]);
        assert_eq!(watches(&store), vec![second.watch, first]);
    }

    #[test]
    fn undo_is_allowed_again_once_the_entry_is_back_where_the_recording_left_it() {
        let (_tmp, dir) = open_tmp();
        let mut store = open(&dir);
        let id = store.add(entry("Show")).unwrap().id;
        let written = store.record(watching(id, 1..=3)).unwrap().watch;
        store.set_progress(id, 5).unwrap();
        store.set_progress(id, 3).unwrap();

        let Recording { entry, watch } = store.undo(written.id).unwrap();
        assert_eq!(entry.progress, 0);
        assert!(recorded(&watch).undone_at.is_some());
    }

    /// Each step breaks the row a little further, so every check in `parse`
    /// is reached with the ones before it passing.
    #[test]
    fn garbage_history_row_surfaces_as_invalid() {
        let (_tmp, dir) = open_tmp();
        let mut store = open(&dir);
        let id = store.add(entry("Show")).unwrap().id;
        let watch = store.record(watching(id, 1..=1)).unwrap().watch.id;
        for (broken, field) in [
            ("UPDATE history SET episode = 5, episode_end = 2", "episode"),
            (
                "UPDATE history SET episode = 1, episode_end = 1, progress = -1",
                "progress",
            ),
            (
                "UPDATE history SET progress = 1, undone_at = -5",
                "undone_at",
            ),
            (
                "UPDATE history SET undone_at = NULL, confidence = 'likely'",
                "link",
            ),
            ("UPDATE history SET confidence = 'bogus'", "confidence"),
            (
                "UPDATE history SET confidence = 'exact', progress = NULL",
                "progress",
            ),
            (
                "UPDATE history SET progress_before = NULL, recorded_at = NULL, undone_at = 5",
                "undone_at",
            ),
            ("UPDATE history SET undone_at = NULL", "outcome"),
            ("UPDATE history SET reason = 'bogus'", "reason"),
            (
                "UPDATE history SET reason = 'not-next', raw_title = NULL",
                "raw_title",
            ),
            ("UPDATE history SET raw_title = 'x', kind = 'bogus'", "kind"),
            // An ignore is nothing being done, so one standing alongside a
            // refusal is a row that cannot be read either way.
            ("UPDATE history SET kind = 'ignored'", "kind"),
        ] {
            store.execute_raw(broken);
            assert!(
                matches!(
                    store.history(None, 10),
                    Err(StoreError::InvalidHistoryRow { id: got, field: got_field })
                        if got == watch && got_field == field
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
    fn a_remembered_title_round_trips_and_survives_reopen() {
        let (_tmp, dir) = open_tmp();
        let mut store = open(&dir);
        let id = store
            .add(entry("Frieren: Beyond Journey's End"))
            .unwrap()
            .id;
        assert_eq!(store.aliases().unwrap(), vec![]);

        store
            .remember_title("sousou no frieren", "Sousou no Frieren", id)
            .unwrap();
        let remembered = vec![("sousou no frieren".to_owned(), id)];
        assert_eq!(store.aliases().unwrap(), remembered);
        drop(store);
        assert_eq!(open(&dir).aliases().unwrap(), remembered);
    }

    #[test]
    fn remembering_the_same_title_again_replaces_the_row() {
        let (_tmp, dir) = open_tmp();
        let mut store = open(&dir);
        let first = store.add(entry("First")).unwrap().id;
        let second = store.add(entry("Second")).unwrap().id;

        store.remember_title("show", "Show", first).unwrap();
        store.remember_title("show", "Show", second).unwrap();
        assert_eq!(store.aliases().unwrap(), vec![("show".to_owned(), second)]);
    }

    #[test]
    fn a_remembered_title_for_an_unknown_entry_is_rejected_by_the_foreign_key() {
        let (_tmp, dir) = open_tmp();
        let mut store = open(&dir);
        assert!(store.remember_title("show", "Show", EntryId(999)).is_err());
        assert_eq!(store.aliases().unwrap(), vec![]);
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
