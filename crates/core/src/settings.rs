use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;
use tracing::{info, info_span, warn};

use crate::{DataDir, Notice, Settings, ThemePreference, error_chain};

/// What booting found in `settings.toml`. A problem never stops the app:
/// the settings are the defaults and the file is left for the user to fix.
#[must_use]
pub(crate) struct Loaded {
    pub(crate) settings: Settings,
    pub(crate) problem: Option<SettingsError>,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum SettingsError {
    #[error("could not read {}", path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("could not parse {}", path.display())]
    Parse {
        path: PathBuf,
        #[source]
        source: TomlError,
    },
    #[error("{} has an invalid {field}: {value:?}", path.display())]
    Invalid {
        path: PathBuf,
        field: &'static str,
        value: String,
    },
    #[error("could not write {}", path.display())]
    Write {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

impl SettingsError {
    pub(crate) fn into_notice(self) -> Notice {
        let detail = error_chain(&self);
        match self {
            SettingsError::Read { .. }
            | SettingsError::Parse { .. }
            | SettingsError::Invalid { .. } => Notice::SettingsUnreadable { detail },
            SettingsError::Write { .. } => Notice::SaveFailed { detail },
        }
    }
}

/// A TOML error whose concrete type stays inside this crate.
#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub(crate) struct TomlError(TomlErrorKind);

#[derive(Debug, thiserror::Error)]
enum TomlErrorKind {
    #[error(transparent)]
    De(#[from] toml::de::Error),
    #[error(transparent)]
    Ser(#[from] toml::ser::Error),
}

/// The file as TOML sees it. Every field is optional and unknown keys pass
/// through, so a hand-edited file survives fields we do not know yet.
#[derive(Serialize, Deserialize, Default)]
struct Document {
    #[serde(default)]
    theme: Option<String>,
}

/// Reads `settings.toml`, writing the defaults when it does not exist yet.
pub(crate) fn load_or_init(dir: &DataDir) -> Loaded {
    let path = dir.settings_file();
    let _span = info_span!("settings.load", path = %path.display()).entered();
    let loaded = match fs::read_to_string(&path) {
        Ok(text) => parse(&path, &text).map(|settings| Loaded {
            settings,
            problem: None,
        }),
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            let settings = Settings::default();
            let problem = write(dir.root(), &path, &settings).err();
            info!("settings.toml missing; wrote defaults");
            Ok(Loaded { settings, problem })
        }
        Err(source) => Err(SettingsError::Read { path, source }),
    };
    match loaded {
        Ok(loaded) => {
            if let Some(problem) = &loaded.problem {
                warn!(error = %error_chain(problem), "default settings not written");
            }
            info!(theme = loaded.settings.theme.tag(), "settings loaded");
            loaded
        }
        Err(problem) => {
            warn!(error = %error_chain(&problem), "settings unreadable; running on defaults");
            Loaded {
                settings: Settings::default(),
                problem: Some(problem),
            }
        }
    }
}

pub(crate) fn save(dir: &DataDir, settings: &Settings) -> Result<(), SettingsError> {
    let path = dir.settings_file();
    let _span =
        info_span!("settings.save", path = %path.display(), theme = settings.theme.tag()).entered();
    write(dir.root(), &path, settings)?;
    info!("settings saved");
    Ok(())
}

fn parse(path: &Path, text: &str) -> Result<Settings, SettingsError> {
    let document: Document = toml::from_str(text).map_err(|err| SettingsError::Parse {
        path: path.to_path_buf(),
        source: TomlError(err.into()),
    })?;
    let theme = match document.theme {
        None => ThemePreference::default(),
        Some(value) => ThemePreference::from_tag(&value).ok_or_else(|| SettingsError::Invalid {
            path: path.to_path_buf(),
            field: "theme",
            value,
        })?,
    };
    Ok(Settings { theme })
}

fn render(path: &Path, settings: &Settings) -> Result<String, SettingsError> {
    let document = Document {
        theme: Some(settings.theme.tag().to_owned()),
    };
    let body = toml::to_string(&document).map_err(|err| SettingsError::Write {
        path: path.to_path_buf(),
        source: io::Error::new(io::ErrorKind::InvalidData, TomlError(err.into())),
    })?;
    Ok(format!("{}{body}", header()))
}

fn header() -> String {
    let themes = ThemePreference::ALL
        .map(|theme| format!("\"{}\"", theme.tag()))
        .join(" | ");
    format!(
        "# Ryuuji settings. Ryuuji rewrites this file whenever a setting changes, \
         so edit it while the app is closed.\n\
         # theme = {themes}\n"
    )
}

fn write(root: &Path, path: &Path, settings: &Settings) -> Result<(), SettingsError> {
    let contents = render(path, settings)?;
    write_atomically(root, path, contents.as_bytes()).map_err(|source| SettingsError::Write {
        path: path.to_path_buf(),
        source,
    })
}

/// The target either keeps its old bytes or holds all the new ones; a crash
/// mid-write leaves only an unnamed temp file beside it.
fn write_atomically(root: &Path, target: &Path, contents: &[u8]) -> io::Result<()> {
    let mut file = NamedTempFile::new_in(root)?;
    file.write_all(contents)?;
    file.as_file().sync_all()?;
    file.persist(target).map_err(|err| err.error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir() -> (tempfile::TempDir, DataDir) {
        let tmp = tempfile::tempdir().unwrap();
        let dir = DataDir::at(tmp.path()).unwrap();
        (tmp, dir)
    }

    fn file_names(dir: &DataDir) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir.root())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn missing_file_is_written_with_defaults_and_header() {
        let (_tmp, dir) = tmp_dir();
        let loaded = load_or_init(&dir);
        assert_eq!(loaded.settings, Settings::default());
        assert!(loaded.problem.is_none());
        let text = fs::read_to_string(dir.settings_file()).unwrap();
        assert!(text.starts_with("# Ryuuji settings."));
        assert!(text.contains("# theme = \"system\" | \"light\" | \"dark\"\n"));
        assert!(text.ends_with("theme = \"system\"\n"));
    }

    #[test]
    fn loading_twice_leaves_the_file_byte_identical() {
        let (_tmp, dir) = tmp_dir();
        let _ = load_or_init(&dir);
        let first = fs::read(dir.settings_file()).unwrap();
        let _ = load_or_init(&dir);
        assert_eq!(fs::read(dir.settings_file()).unwrap(), first);
    }

    #[test]
    fn save_then_load_round_trips_without_leftovers() {
        let (_tmp, dir) = tmp_dir();
        let dark = Settings {
            theme: ThemePreference::Dark,
        };
        save(&dir, &dark).unwrap();
        let loaded = load_or_init(&dir);
        assert_eq!(loaded.settings, dark);
        assert!(loaded.problem.is_none());
        assert_eq!(file_names(&dir), ["logs", "settings.toml"]);
    }

    #[test]
    fn invalid_toml_is_a_parse_problem_and_stays_untouched() {
        let (_tmp, dir) = tmp_dir();
        let garbage = b"theme = [unterminated";
        fs::write(dir.settings_file(), garbage).unwrap();
        let loaded = load_or_init(&dir);
        assert_eq!(loaded.settings, Settings::default());
        assert!(matches!(loaded.problem, Some(SettingsError::Parse { .. })));
        assert_eq!(fs::read(dir.settings_file()).unwrap(), garbage);
    }

    #[test]
    fn unknown_theme_is_an_invalid_problem_and_stays_untouched() {
        let (_tmp, dir) = tmp_dir();
        let text = "theme = \"blue\"\n";
        fs::write(dir.settings_file(), text).unwrap();
        let loaded = load_or_init(&dir);
        assert_eq!(loaded.settings, Settings::default());
        assert!(matches!(
            loaded.problem,
            Some(SettingsError::Invalid { field: "theme", ref value, .. }) if value == "blue"
        ));
        assert_eq!(fs::read_to_string(dir.settings_file()).unwrap(), text);
    }

    #[test]
    fn unknown_keys_are_tolerated() {
        let (_tmp, dir) = tmp_dir();
        fs::write(dir.settings_file(), "future_knob = 3\n").unwrap();
        let loaded = load_or_init(&dir);
        assert_eq!(loaded.settings, Settings::default());
        assert!(loaded.problem.is_none());
    }

    #[test]
    fn save_over_a_directory_is_a_write_error() {
        let (_tmp, dir) = tmp_dir();
        fs::create_dir(dir.settings_file()).unwrap();
        let result = save(&dir, &Settings::default());
        assert!(matches!(result, Err(SettingsError::Write { .. })));
        assert!(dir.settings_file().is_dir());
        assert_eq!(file_names(&dir), ["logs", "settings.toml"]);
    }
}
