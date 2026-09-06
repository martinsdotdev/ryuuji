use std::path::{Path, PathBuf};
use std::time::SystemTime;
use std::{fmt, fs, io};

use tracing::{debug, debug_span};

use crate::{DataDir, DataDirSource, SchemaVersion, Store, error_chain};

/// A byte count that prints in binary units.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ByteSize(pub u64);

impl fmt::Display for ByteSize {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
        let mut value = self.0 as f64;
        let mut unit = 0;
        while value >= 1024.0 && unit < UNITS.len() - 1 {
            value /= 1024.0;
            unit += 1;
        }
        if unit == 0 {
            write!(f, "{} B", self.0)
        } else {
            write!(f, "{value:.1} {}", UNITS[unit])
        }
    }
}

/// What `fs::metadata` said about one of Ryuuji's files.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileStat {
    Present {
        len: ByteSize,
        modified: Option<SystemTime>,
    },
    Missing,
    Unreadable(io::ErrorKind),
}

impl FileStat {
    fn of(path: &Path) -> FileStat {
        match fs::metadata(path) {
            Ok(meta) => FileStat::Present {
                len: ByteSize(meta.len()),
                modified: meta.modified().ok(),
            },
            Err(err) if err.kind() == io::ErrorKind::NotFound => FileStat::Missing,
            Err(err) => FileStat::Unreadable(err.kind()),
        }
    }
}

/// A file Ryuuji expects and what was found there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileFacts {
    pub path: PathBuf,
    pub stat: FileStat,
}

impl FileFacts {
    fn of(path: PathBuf) -> FileFacts {
        let stat = FileStat::of(&path);
        FileFacts { path, stat }
    }
}

/// The schema probe failed; `detail` is the error chain.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProbeFailed {
    pub detail: String,
}

/// A snapshot of where Ryuuji's files are and what state they are in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostics {
    pub data_dir: PathBuf,
    pub data_dir_source: DataDirSource,
    pub library: FileFacts,
    pub schema: Result<SchemaVersion, ProbeFailed>,
    pub settings: FileFacts,
    pub logs: PathBuf,
    /// The newest file in `logs`, when there is one.
    pub current_log: Option<FileFacts>,
}

impl Diagnostics {
    /// Reads file metadata and `PRAGMA user_version` only. Never reads
    /// file contents and never runs a check that could write.
    pub fn gather(dir: &DataDir, store: &Store) -> Diagnostics {
        let _span = debug_span!("diagnostics.gather").entered();
        let schema = store.schema_version().map_err(|err| ProbeFailed {
            detail: error_chain(&err),
        });
        let logs = dir.logs();
        let current_log = newest_file(&logs).map(FileFacts::of);
        let diagnostics = Diagnostics {
            data_dir: dir.root().to_path_buf(),
            data_dir_source: dir.source(),
            library: FileFacts::of(dir.library_db()),
            schema,
            settings: FileFacts::of(dir.settings_file()),
            logs,
            current_log,
        };
        debug!(
            library = ?diagnostics.library.stat,
            settings = ?diagnostics.settings.stat,
            "diagnostics gathered"
        );
        diagnostics
    }
}

fn newest_file(dir: &Path) -> Option<PathBuf> {
    fs::read_dir(dir)
        .ok()?
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let meta = entry.metadata().ok()?;
            meta.is_file().then(|| (meta.modified().ok(), entry.path()))
        })
        .max_by_key(|(modified, _)| *modified)
        .map(|(_, path)| path)
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, UNIX_EPOCH};

    use super::*;
    use crate::{Opened, settings};

    fn open_tmp() -> (tempfile::TempDir, DataDir, Store) {
        let tmp = tempfile::tempdir().unwrap();
        let dir = DataDir::at(tmp.path()).unwrap();
        let Opened { store, recovered } = Store::open(&dir).unwrap();
        assert_eq!(recovered, None);
        (tmp, dir, store)
    }

    fn write_with_mtime(path: &Path, unix_secs: u64) {
        fs::write(path, b"log line\n").unwrap();
        fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(UNIX_EPOCH + Duration::from_secs(unix_secs))
            .unwrap();
    }

    #[test]
    fn gather_reports_the_library_and_its_schema() {
        let (_tmp, dir, store) = open_tmp();
        let diagnostics = Diagnostics::gather(&dir, &store);
        assert_eq!(diagnostics.data_dir, dir.root());
        assert_eq!(diagnostics.data_dir_source, DataDirSource::Explicit);
        assert_eq!(diagnostics.library.path, dir.library_db());
        assert!(matches!(
            diagnostics.library.stat,
            FileStat::Present { len, modified: Some(_) } if len > ByteSize(0)
        ));
        assert_eq!(diagnostics.schema, Ok(SchemaVersion(5)));
        assert_eq!(diagnostics.logs, dir.logs());
    }

    #[test]
    fn gather_twice_is_equal() {
        let (_tmp, dir, store) = open_tmp();
        assert_eq!(
            Diagnostics::gather(&dir, &store),
            Diagnostics::gather(&dir, &store)
        );
    }

    #[test]
    fn settings_file_is_missing_until_loaded() {
        let (_tmp, dir, store) = open_tmp();
        let before = Diagnostics::gather(&dir, &store);
        assert_eq!(before.settings.path, dir.settings_file());
        assert_eq!(before.settings.stat, FileStat::Missing);

        let _ = settings::load_or_init(&dir);
        let after = Diagnostics::gather(&dir, &store);
        assert!(matches!(
            after.settings.stat,
            FileStat::Present { len, .. } if len > ByteSize(0)
        ));
    }

    #[test]
    fn current_log_is_the_newest_file_or_none() {
        let (_tmp, dir, store) = open_tmp();
        assert_eq!(Diagnostics::gather(&dir, &store).current_log, None);

        let older = dir.logs().join("ryuuji.log.2026-08-27");
        let newer = dir.logs().join("ryuuji.log.2026-08-28");
        write_with_mtime(&newer, 1_756_400_000);
        write_with_mtime(&older, 1_756_300_000);

        let current = Diagnostics::gather(&dir, &store).current_log.unwrap();
        assert_eq!(current.path, newer);
        assert_eq!(
            current.stat,
            FileStat::Present {
                len: ByteSize(9),
                modified: Some(UNIX_EPOCH + Duration::from_secs(1_756_400_000)),
            }
        );
    }

    #[test]
    fn byte_size_prints_binary_units() {
        assert_eq!(ByteSize(0).to_string(), "0 B");
        assert_eq!(ByteSize(1023).to_string(), "1023 B");
        assert_eq!(ByteSize(1024).to_string(), "1.0 KiB");
        assert_eq!(ByteSize(1536).to_string(), "1.5 KiB");
        assert_eq!(ByteSize(5 * 1024 * 1024).to_string(), "5.0 MiB");
        assert_eq!(ByteSize(3 * 1024 * 1024 * 1024).to_string(), "3.0 GiB");
    }

    #[test]
    fn gather_leaves_the_library_file_untouched() {
        let (_tmp, dir, store) = open_tmp();
        let before = fs::metadata(dir.library_db()).unwrap();
        for _ in 0..3 {
            let _ = Diagnostics::gather(&dir, &store);
        }
        let after = fs::metadata(dir.library_db()).unwrap();
        assert_eq!(after.len(), before.len());
        assert_eq!(after.modified().unwrap(), before.modified().unwrap());
    }
}
