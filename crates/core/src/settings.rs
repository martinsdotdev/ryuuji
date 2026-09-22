//! `settings.toml`, read and written under the rule in [`crate::user_file`].

use std::{fs, io};

use tracing::{info, info_span};

use crate::user_file::{self, Loaded, UserFile, WriteError};
use crate::{DataDir, Settings, ThemePreference};

const KEYS: &[&str] = &["theme"];

#[derive(Debug, thiserror::Error)]
pub(crate) enum SaveError {
    #[error("settings.toml couldn't be read, so the theme wasn't saved")]
    Read(#[source] io::Error),
    /// A reread of such a file keeps what is running, so the pick lasts
    /// until the file parses again.
    #[error(
        "settings.toml isn't valid TOML, so the theme wasn't saved. It applies until the file \
         is fixed"
    )]
    NotToml,
    #[error("settings.toml couldn't be written, so the theme wasn't saved")]
    Write(#[from] WriteError),
}

/// Reads `settings.toml`, writing a starter when it does not exist yet.
pub(crate) fn load(dir: &DataDir) -> Loaded<Settings> {
    user_file::load(dir, UserFile::Settings, &render(&Settings::default()), read)
}

/// Writes `theme` into `settings.toml` and changes nothing else: the
/// person's comments, keys Ryuuji does not know and values it could not
/// read all stay as written, and a theme it could not read gives way to the
/// pick. A file that is not TOML is left alone.
pub(crate) fn save_theme(dir: &DataDir, theme: ThemePreference) -> Result<(), SaveError> {
    let _span = info_span!("settings.save", theme = theme.tag()).entered();
    let path = UserFile::Settings.path(dir);
    let text = match fs::read_to_string(&path) {
        Ok(text) => {
            let mut document: toml_edit::DocumentMut =
                text.parse().map_err(|_| SaveError::NotToml)?;
            document["theme"] = toml_edit::value(theme.tag());
            document.to_string()
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => render(&Settings { theme }),
        Err(err) => return Err(SaveError::Read(err)),
    };
    user_file::write(dir, &path, text.as_bytes())?;
    info!("theme saved");
    Ok(())
}

fn read(table: toml::Table, problems: &mut Vec<String>) -> Settings {
    let theme = match table.get("theme") {
        None => ThemePreference::default(),
        Some(toml::Value::String(tag)) => ThemePreference::from_tag(tag).unwrap_or_else(|| {
            problems.push(format!(
                "theme = {tag:?} isn't one of {}, so it is {:?}.",
                themes(),
                ThemePreference::default().tag()
            ));
            ThemePreference::default()
        }),
        Some(value) => {
            problems.push(format!(
                "theme should be text, not {}, so it is {:?}.",
                value.type_str(),
                ThemePreference::default().tag()
            ));
            ThemePreference::default()
        }
    };
    problems.extend(
        user_file::unknown_keys(&table, KEYS)
            .map(|key| format!("{key} isn't a setting Ryuuji knows, so it is ignored.")),
    );
    Settings { theme }
}

fn render(settings: &Settings) -> String {
    format!(
        "# Ryuuji settings. A save applies at once. Changing a setting in Ryuuji \
         changes only its own line here.\n\
         # theme = {}\n\
         theme = {:?}\n",
        themes(),
        settings.theme.tag()
    )
}

fn themes() -> String {
    ThemePreference::ALL
        .map(|theme| format!("\"{}\"", theme.tag()))
        .join(" | ")
}

#[cfg(test)]
mod tests {
    use std::fs;

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

    fn dark() -> Settings {
        Settings {
            theme: ThemePreference::Dark,
        }
    }

    #[test]
    fn missing_file_is_written_with_defaults_and_header() {
        let (_tmp, dir) = tmp_dir();
        let loaded = load(&dir);
        assert_eq!(loaded.value, Some(Settings::default()));
        assert!(loaded.is_clean());
        let text = fs::read_to_string(dir.settings_file()).unwrap();
        assert!(text.starts_with("# Ryuuji settings."));
        assert!(text.contains("# theme = \"system\" | \"light\" | \"dark\"\n"));
        assert!(text.ends_with("theme = \"system\"\n"));
    }

    #[test]
    fn loading_twice_leaves_the_file_byte_identical() {
        let (_tmp, dir) = tmp_dir();
        let _ = load(&dir);
        let first = fs::read(dir.settings_file()).unwrap();
        let _ = load(&dir);
        assert_eq!(fs::read(dir.settings_file()).unwrap(), first);
    }

    #[test]
    fn save_then_load_round_trips_without_leftovers() {
        let (_tmp, dir) = tmp_dir();
        let _ = load(&dir);
        save_theme(&dir, ThemePreference::Dark).unwrap();
        let loaded = load(&dir);
        assert_eq!(loaded.value, Some(dark()));
        assert!(loaded.is_clean());
        assert_eq!(file_names(&dir), ["logs", "settings.toml"]);
    }

    #[test]
    fn a_theme_save_to_a_missing_file_writes_the_starter_with_it() {
        let (_tmp, dir) = tmp_dir();
        save_theme(&dir, ThemePreference::Dark).unwrap();
        let text = fs::read_to_string(dir.settings_file()).unwrap();
        assert!(text.starts_with("# Ryuuji settings."));
        assert!(text.ends_with("theme = \"dark\"\n"));
    }

    /// The person's file is theirs: a pick rewrites its own line and
    /// nothing else.
    #[test]
    fn a_theme_save_keeps_comments_unknown_keys_and_order() {
        let (_tmp, dir) = tmp_dir();
        fs::write(
            dir.settings_file(),
            "# mine\nfuture_knob = 3 # later\ntheme = \"light\"\n\n# end\n",
        )
        .unwrap();
        save_theme(&dir, ThemePreference::Dark).unwrap();
        assert_eq!(
            fs::read_to_string(dir.settings_file()).unwrap(),
            "# mine\nfuture_knob = 3 # later\ntheme = \"dark\"\n\n# end\n"
        );
    }

    #[test]
    fn a_theme_save_replaces_a_theme_it_could_not_read() {
        let (_tmp, dir) = tmp_dir();
        fs::write(dir.settings_file(), "theme = \"blue\"\n").unwrap();
        save_theme(&dir, ThemePreference::Dark).unwrap();
        assert_eq!(
            fs::read_to_string(dir.settings_file()).unwrap(),
            "theme = \"dark\"\n"
        );
    }

    #[test]
    fn a_theme_save_leaves_a_file_that_is_not_toml_alone() {
        let (_tmp, dir) = tmp_dir();
        let garbage = b"theme = [unterminated";
        fs::write(dir.settings_file(), garbage).unwrap();
        assert!(matches!(
            save_theme(&dir, ThemePreference::Dark),
            Err(SaveError::NotToml)
        ));
        assert_eq!(fs::read(dir.settings_file()).unwrap(), garbage);
    }

    #[test]
    fn invalid_toml_applies_nothing_and_stays_untouched() {
        let (_tmp, dir) = tmp_dir();
        let garbage = b"theme = [unterminated";
        fs::write(dir.settings_file(), garbage).unwrap();
        let loaded = load(&dir);
        assert_eq!(loaded.value, None);
        assert_eq!(loaded.problems.len(), 1);
        assert_eq!(fs::read(dir.settings_file()).unwrap(), garbage);
    }

    #[test]
    fn unknown_theme_falls_back_names_the_value_and_stays_untouched() {
        let (_tmp, dir) = tmp_dir();
        let text = "theme = \"blue\"\n";
        fs::write(dir.settings_file(), text).unwrap();
        let loaded = load(&dir);
        assert_eq!(loaded.value, Some(Settings::default()));
        assert_eq!(
            loaded.problems,
            [
                "theme = \"blue\" isn't one of \"system\" | \"light\" | \"dark\", so it is \"system\"."
            ]
        );
        assert_eq!(fs::read_to_string(dir.settings_file()).unwrap(), text);
    }

    #[test]
    fn a_theme_that_is_not_text_falls_back() {
        let (_tmp, dir) = tmp_dir();
        fs::write(dir.settings_file(), "theme = 3\n").unwrap();
        let loaded = load(&dir);
        assert_eq!(loaded.value, Some(Settings::default()));
        assert_eq!(
            loaded.problems,
            ["theme should be text, not integer, so it is \"system\"."]
        );
    }

    /// A newer build's key is named, not fatal: the theme beside it still
    /// applies.
    #[test]
    fn an_unknown_key_is_named_and_the_theme_beside_it_applies() {
        let (_tmp, dir) = tmp_dir();
        fs::write(dir.settings_file(), "theme = \"dark\"\nfuture_knob = 3\n").unwrap();
        let loaded = load(&dir);
        assert_eq!(loaded.value, Some(dark()));
        assert_eq!(
            loaded.problems,
            ["future_knob isn't a setting Ryuuji knows, so it is ignored."]
        );
    }

    #[test]
    fn a_theme_save_over_a_directory_is_refused() {
        let (_tmp, dir) = tmp_dir();
        fs::create_dir(dir.settings_file()).unwrap();
        assert!(save_theme(&dir, ThemePreference::Dark).is_err());
        assert!(dir.settings_file().is_dir());
        assert_eq!(file_names(&dir), ["logs", "settings.toml"]);
    }
}
