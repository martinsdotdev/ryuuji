//! What every file a person may edit has in common, modelled on Windows
//! Terminal's settings. The built-ins live in the program and the file holds
//! only what the person wrote. A missing file gets a starter that explains
//! it. A problem never stops the app and drops only the part it is in: a bad
//! value or row falls back to the built-in under it, an unknown key is
//! ignored, and a file that is not TOML at all applies nothing. Every
//! problem is said, once per file, in words the person can act on. When a
//! setting changes in the app, Ryuuji writes that one value and leaves the
//! rest of the file as the person wrote it, and it never writes a file that
//! is not TOML, since it cannot rewrite what it did not understand without
//! destroying it.
//!
//! App state is not a file a person edits: the library is Ryuuji's own, and
//! a damaged one is moved aside rather than read around.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use tempfile::NamedTempFile;
use tracing::{info, info_span, warn};

use crate::DataDir;
use crate::tagged::tagged_enum;

tagged_enum! {
    /// A file in the data folder that a person may edit. The tag is its
    /// file name.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    pub enum UserFile {
        Settings => "settings.toml", "Settings",
    }
}

impl UserFile {
    pub fn file_name(self) -> &'static str {
        self.tag()
    }

    pub(crate) fn path(self, dir: &DataDir) -> PathBuf {
        dir.root().join(self.file_name())
    }
}

/// What one read of a person's file found.
#[must_use]
pub(crate) struct Loaded<T> {
    /// What the file says over the built-ins, or `None` when it could not be
    /// read or is not TOML, so nothing in it applies. A missing file is the
    /// built-ins: the person wrote nothing.
    pub(crate) value: Option<T>,
    /// One sentence per problem, each saying what was dropped.
    pub(crate) problems: Vec<String>,
}

impl<T> Loaded<T> {
    /// Whether the file can be written without losing anything in it.
    pub(crate) fn is_clean(&self) -> bool {
        self.problems.is_empty()
    }
}

#[derive(Debug, thiserror::Error)]
#[error("could not write {}", path.display())]
pub(crate) struct WriteError {
    path: PathBuf,
    #[source]
    source: io::Error,
}

/// Reads `file` and hands its table to `read`, which takes what it
/// understands and pushes a sentence for everything it drops. A missing file
/// gets `starter` and reads as `T::default()`.
pub(crate) fn load<T: Default>(
    dir: &DataDir,
    file: UserFile,
    starter: &str,
    read: impl FnOnce(toml::Table, &mut Vec<String>) -> T,
) -> Loaded<T> {
    let path = file.path(dir);
    let _span = info_span!("user_file.load", file = file.file_name()).entered();
    let loaded = match fs::read_to_string(&path) {
        Ok(text) => match text.parse::<toml::Table>() {
            Ok(table) => {
                let mut problems = Vec::new();
                let value = read(table, &mut problems);
                Loaded {
                    value: Some(value),
                    problems,
                }
            }
            Err(err) => Loaded {
                value: None,
                problems: vec![format!(
                    "It isn't valid TOML ({}), so none of it applies.",
                    toml_error(&text, &err)
                )],
            },
        },
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            match write(dir, &path, starter.as_bytes()) {
                Ok(()) => info!("missing; wrote the starter"),
                // The person did nothing, so this is not theirs to hear.
                Err(err) => warn!(error = %crate::error_chain(&err), "starter not written"),
            }
            Loaded {
                value: Some(T::default()),
                problems: Vec::new(),
            }
        }
        Err(err) => Loaded {
            value: None,
            problems: vec![format!(
                "It couldn't be read ({err}), so none of it applies."
            )],
        },
    };
    if loaded.is_clean() {
        info!("loaded");
    } else {
        warn!(problems = ?loaded.problems, "loaded with problems");
    }
    loaded
}

/// The keys of `table` that are not in `known`, in the table's order.
pub(crate) fn unknown_keys<'a>(
    table: &'a toml::Table,
    known: &'a [&str],
) -> impl Iterator<Item = &'a str> {
    table
        .keys()
        .map(String::as_str)
        .filter(|key| !known.contains(key))
}

/// Replaces the file in one step: the target either keeps its old bytes or
/// holds all the new ones, and a crash mid-write leaves only an unnamed temp
/// file beside it.
pub(crate) fn write(dir: &DataDir, path: &Path, contents: &[u8]) -> Result<(), WriteError> {
    let written = || -> io::Result<()> {
        let mut file = NamedTempFile::new_in(dir.root())?;
        file.write_all(contents)?;
        file.as_file().sync_all()?;
        file.persist(path).map_err(|err| err.error)?;
        Ok(())
    };
    written().map_err(|source| WriteError {
        path: path.to_path_buf(),
        source,
    })
}

/// Where the parser stopped and why, on one line: the TOML error's own
/// display draws the line with a caret under it, which a notice cannot show.
fn toml_error(text: &str, err: &toml::de::Error) -> String {
    let message = err.message().trim().replace('\n', ", ");
    let Some(span) = err.span() else {
        return message;
    };
    // A span that does not fall on a character boundary leaves the message
    // unplaced rather than panicking inside the reducer.
    let Some(before) = text.get(..span.start) else {
        return message;
    };
    let line = before.matches('\n').count() + 1;
    let column = before
        .rfind('\n')
        .map_or(before, |at| &before[at + 1..])
        .chars()
        .count()
        + 1;
    format!("line {line}, column {column}: {message}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir() -> (tempfile::TempDir, DataDir) {
        let tmp = tempfile::tempdir().unwrap();
        let dir = DataDir::at(tmp.path()).unwrap();
        (tmp, dir)
    }

    fn keys(table: toml::Table, problems: &mut Vec<String>) -> Vec<String> {
        problems
            .extend(unknown_keys(&table, &["known"]).map(|key| format!("`{key}` is not known")));
        table.keys().cloned().collect()
    }

    #[test]
    fn a_missing_file_gets_the_starter_and_reads_as_the_default() {
        let (_tmp, dir) = tmp_dir();
        let loaded = load(&dir, UserFile::Settings, "# starter\n", keys);
        assert_eq!(loaded.value, Some(Vec::new()));
        assert!(loaded.is_clean());
        assert_eq!(
            fs::read_to_string(dir.settings_file()).unwrap(),
            "# starter\n"
        );
    }

    #[test]
    fn a_starter_that_cannot_be_written_is_not_a_problem() {
        let (tmp, dir) = tmp_dir();
        fs::remove_dir_all(tmp.path()).unwrap();
        let loaded = load(&dir, UserFile::Settings, "# starter\n", keys);
        assert_eq!(loaded.value, Some(Vec::new()));
        assert!(loaded.is_clean());
    }

    #[test]
    fn a_file_that_is_not_toml_applies_nothing_and_says_where() {
        let (_tmp, dir) = tmp_dir();
        let text = "known = 1\nbroken = [unterminated\n";
        fs::write(dir.settings_file(), text).unwrap();
        let loaded = load(&dir, UserFile::Settings, "", keys);
        assert_eq!(loaded.value, None);
        assert_eq!(loaded.problems.len(), 1);
        assert!(
            loaded.problems[0].contains("line 2, column 23: unclosed array"),
            "{:?}",
            loaded.problems
        );
        assert!(!loaded.problems[0].contains('\n'));
        assert_eq!(fs::read_to_string(dir.settings_file()).unwrap(), text);
    }

    #[test]
    fn a_file_that_cannot_be_read_applies_nothing() {
        let (_tmp, dir) = tmp_dir();
        fs::create_dir(dir.settings_file()).unwrap();
        let loaded = load(&dir, UserFile::Settings, "", keys);
        assert_eq!(loaded.value, None);
        assert!(loaded.problems[0].starts_with("It couldn't be read"));
    }

    #[test]
    fn what_the_reader_drops_is_a_problem_and_the_rest_applies() {
        let (_tmp, dir) = tmp_dir();
        fs::write(dir.settings_file(), "known = 1\nother = 2\n").unwrap();
        let loaded = load(&dir, UserFile::Settings, "", keys);
        assert_eq!(
            loaded.value,
            Some(vec!["known".to_owned(), "other".to_owned()])
        );
        assert_eq!(loaded.problems, ["`other` is not known"]);
    }
}
