//! `players.toml`: the players a person adds, changes or turns off, read
//! under the rule in [`crate::user_file`]. The built-in table lives in the
//! detection crate, which lays these rows over it; this module only reads
//! what the person wrote.

use crate::DataDir;
use crate::user_file::{self, Loaded, UserFile};

/// The shortest pattern a player row may hold. Shorter ones would claim
/// sessions that merely contain them.
pub const MIN_PATTERN_LEN: usize = 3;

/// Whether `pattern` is too short to identify a player. The one rule every
/// player list is held to, the built-in table's and discovery's included.
pub fn too_short(pattern: &str) -> bool {
    pattern.chars().count() < MIN_PATTERN_LEN
}

/// One `[[player]]` row as the person wrote it. A row named like a built-in
/// player changes only the lists it writes, so each list keeps "left out"
/// (`None`) apart from "emptied" (`Some` of nothing). Only a read makes one,
/// so every row is checked, trimmed and lowercased by the time anyone holds
/// it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlayerRow {
    pub(crate) name: String,
    pub(crate) smtc_app_ids: Option<Vec<String>>,
    pub(crate) mpris_ids: Option<Vec<String>>,
    pub(crate) executables: Option<Vec<String>>,
    pub(crate) hidden: bool,
}

impl PlayerRow {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn smtc_app_ids(&self) -> Option<&[String]> {
        self.smtc_app_ids.as_deref()
    }

    pub fn mpris_ids(&self) -> Option<&[String]> {
        self.mpris_ids.as_deref()
    }

    pub fn executables(&self) -> Option<&[String]> {
        self.executables.as_deref()
    }

    /// The player is known but never watched.
    pub fn hidden(&self) -> bool {
        self.hidden
    }

    /// The rows `text` would load as `players.toml`, without the problems.
    /// Nothing is left if it is not TOML.
    pub fn read_all(text: &str) -> Vec<PlayerRow> {
        text.parse::<toml::Table>()
            .map(|table| read(table, &mut Vec::new()))
            .unwrap_or_default()
    }
}

const FILE_KEYS: &[&str] = &["player"];
const ROW_KEYS: &[&str] = &["name", "smtc_app_ids", "mpris_ids", "executables", "hidden"];

const STARTER: &str = "\
# Players Ryuuji detects, on top of the ones it knows already.
#
# Ryuuji reads this file when it starts and never writes to it.
# Settings > Diagnostics > Media sessions lists every player session with its
# app id and the player it matched, which is where to find what to write here.
# A row holds a name and any of smtc_app_ids (Windows), mpris_ids (Linux),
# executables and hidden.
#
# Add a player Ryuuji doesn't know:
#
# [[player]]
# name = \"PotPlayer\"
# smtc_app_ids = [\"potplayer\"]
# executables = [\"potplayermini64.exe\"]
#
# Change a player Ryuuji knows by using its name. The lists you write replace
# its own, and an empty list clears one. Without executables, which window is
# in front says nothing about whether you are watching:
#
# [[player]]
# name = \"mpv\"
# executables = []
#
# Turn a player off:
#
# [[player]]
# name = \"Brave\"
# hidden = true
";

/// Reads `players.toml`, writing the starter when it does not exist yet.
pub(crate) fn load(dir: &DataDir) -> Loaded<Vec<PlayerRow>> {
    user_file::load(dir, UserFile::Players, STARTER, read)
}

fn read(mut table: toml::Table, problems: &mut Vec<String>) -> Vec<PlayerRow> {
    problems.extend(
        user_file::unknown_keys(&table, FILE_KEYS)
            .map(|key| format!("{key} isn't something players.toml holds, so it is ignored.")),
    );
    let rows = match table.remove("player") {
        None => return Vec::new(),
        Some(toml::Value::Array(rows)) => rows,
        Some(other) => {
            problems.push(format!(
                "player should be [[player]] rows, not {}, so no row applies.",
                other.type_str()
            ));
            return Vec::new();
        }
    };
    let mut players: Vec<PlayerRow> = Vec::new();
    for (index, row) in rows.into_iter().enumerate() {
        let label = match row.get("name").and_then(toml::Value::as_str) {
            Some(name) => format!("Player {name:?}"),
            None => format!("Player {}", index + 1),
        };
        if let toml::Value::Table(fields) = &row {
            problems.extend(user_file::unknown_keys(fields, ROW_KEYS).map(|key| {
                format!("{label}: {key} isn't something a player holds, so it is ignored.")
            }));
        }
        let mut player = match player(&row).and_then(|player| usable(player, &players)) {
            Ok(player) => player,
            Err(reason) => {
                problems.push(format!("{label}: {reason}, so the row is ignored."));
                continue;
            }
        };
        for pattern in patterns(&mut player) {
            *pattern = pattern.to_lowercase();
        }
        players.push(player);
    }
    players
}

/// The row's fields, or why one of them cannot be read. Read by hand rather
/// than through serde so the reason names the field.
fn player(row: &toml::Value) -> Result<PlayerRow, String> {
    let Some(fields) = row.as_table() else {
        return Err(format!(
            "it should be a [[player]] table, not {}",
            row.type_str()
        ));
    };
    let name = match fields.get("name") {
        Some(toml::Value::String(name)) => name.trim().to_owned(),
        Some(other) => return Err(format!("name should be text, not {}", other.type_str())),
        None => return Err("it has no name".to_owned()),
    };
    let hidden = match fields.get("hidden") {
        None => false,
        Some(toml::Value::Boolean(hidden)) => *hidden,
        Some(other) => {
            return Err(format!(
                "hidden should be true or false, not {}",
                other.type_str()
            ));
        }
    };
    Ok(PlayerRow {
        name,
        smtc_app_ids: list(fields, "smtc_app_ids")?,
        mpris_ids: list(fields, "mpris_ids")?,
        executables: list(fields, "executables")?,
        hidden,
    })
}

fn list(fields: &toml::Table, key: &str) -> Result<Option<Vec<String>>, String> {
    let items = match fields.get(key) {
        None => return Ok(None),
        Some(toml::Value::Array(items)) => items,
        Some(other) => {
            return Err(format!(
                "{key} should be a list like [\"mpv.exe\"], not {}",
                other.type_str()
            ));
        }
    };
    items
        .iter()
        .map(|item| {
            item.as_str()
                .map(|pattern| pattern.trim().to_owned())
                .ok_or_else(|| format!("{key} should hold text, not {}", item.type_str()))
        })
        .collect::<Result<_, _>>()
        .map(Some)
}

/// The row, unless it cannot be used beside the rows before it.
fn usable(player: PlayerRow, earlier: &[PlayerRow]) -> Result<PlayerRow, String> {
    if player.name.is_empty() {
        return Err("its name is empty".to_owned());
    }
    // Names meet built-in names without regard to case, so they meet each
    // other the same way.
    let name = player.name.to_lowercase();
    if earlier.iter().any(|p| p.name.to_lowercase() == name) {
        return Err("an earlier row has the same name".to_owned());
    }
    for (field, list) in [
        ("smtc_app_ids", &player.smtc_app_ids),
        ("mpris_ids", &player.mpris_ids),
        ("executables", &player.executables),
    ] {
        if let Some(short) = list.iter().flatten().find(|pattern| too_short(pattern)) {
            return Err(format!(
                "{field} holds {short:?}, shorter than {MIN_PATTERN_LEN} characters"
            ));
        }
    }
    Ok(player)
}

fn patterns(player: &mut PlayerRow) -> impl Iterator<Item = &mut String> {
    [
        &mut player.smtc_app_ids,
        &mut player.mpris_ids,
        &mut player.executables,
    ]
    .into_iter()
    .flatten()
    .flatten()
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

    fn loaded(text: &str) -> Loaded<Vec<PlayerRow>> {
        let (_tmp, dir) = tmp_dir();
        fs::write(dir.players_file(), text).unwrap();
        load(&dir)
    }

    fn names(rows: &[PlayerRow]) -> Vec<&str> {
        rows.iter().map(|row| row.name.as_str()).collect()
    }

    #[test]
    fn a_missing_file_gets_the_starter_and_no_rows() {
        let (_tmp, dir) = tmp_dir();
        let first = load(&dir);
        assert_eq!(first.value, Some(Vec::new()));
        assert!(first.is_clean());
        let text = fs::read_to_string(dir.players_file()).unwrap();
        assert_eq!(text, STARTER);
        let _ = load(&dir);
        assert_eq!(fs::read_to_string(dir.players_file()).unwrap(), text);
    }

    /// The starter's examples are the documentation, so they have to load
    /// as written once their comment marks come off.
    #[test]
    fn the_starter_examples_load_cleanly_when_uncommented() {
        let uncommented: String = STARTER
            .lines()
            .filter_map(|line| line.strip_prefix("# "))
            .filter(|rest| {
                rest.starts_with("[[")
                    || rest.split_once(" = ").is_some_and(|(key, _)| {
                        key.chars().all(|c| c.is_ascii_lowercase() || c == '_')
                    })
            })
            .map(|line| format!("{line}\n"))
            .collect();
        let loaded = loaded(&uncommented);
        assert!(loaded.is_clean(), "{:?}", loaded.problems);
        let rows = loaded.value.unwrap();
        assert_eq!(names(&rows), ["PotPlayer", "mpv", "Brave"]);
        assert_eq!(rows[1].executables, Some(Vec::new()));
        assert!(rows[2].hidden);
    }

    #[test]
    fn a_file_that_is_not_toml_gives_no_rows_and_stays_untouched() {
        let (_tmp, dir) = tmp_dir();
        let text = "[[player]]\nname = \"mpv\n";
        fs::write(dir.players_file(), text).unwrap();
        let loaded = load(&dir);
        assert_eq!(loaded.value, None);
        assert_eq!(loaded.problems.len(), 1);
        assert_eq!(fs::read_to_string(dir.players_file()).unwrap(), text);
    }

    #[test]
    fn a_bad_row_is_dropped_alone_and_named() {
        let loaded = loaded(
            "[[player]]\nname = \"PotPlayer\"\nsmtc_app_ids = [\"potplayer\"]\n\
             [[player]]\nname = \"Odd\"\nsmtc_app_ids = \"odd.exe\"\n\
             [[player]]\nname = \"mpv\"\nhidden = true\n",
        );
        assert_eq!(names(&loaded.value.unwrap()), ["PotPlayer", "mpv"]);
        assert_eq!(
            loaded.problems,
            [
                "Player \"Odd\": smtc_app_ids should be a list like [\"mpv.exe\"], not string, so the row is ignored."
            ]
        );
    }

    #[test]
    fn a_row_without_a_name_is_named_by_its_place() {
        let loaded = loaded("[[player]]\nhidden = true\n");
        assert_eq!(loaded.value, Some(Vec::new()));
        assert_eq!(
            loaded.problems,
            ["Player 1: it has no name, so the row is ignored."]
        );
    }

    #[test]
    fn an_unknown_key_is_named_and_the_row_kept() {
        let loaded = loaded(
            "stray = 1\n[[player]]\nname = \"mpv\"\nsmtc_app_id = [\"mpv.exe\"]\nhidden = true\n",
        );
        assert_eq!(names(&loaded.value.unwrap()), ["mpv"]);
        assert_eq!(
            loaded.problems,
            [
                "stray isn't something players.toml holds, so it is ignored.",
                "Player \"mpv\": smtc_app_id isn't something a player holds, so it is ignored.",
            ]
        );
    }

    #[test]
    fn a_short_pattern_drops_its_row_and_names_the_field() {
        let loaded = loaded("[[player]]\nname = \"x\"\nexecutables = [\"ab\"]\n");
        assert_eq!(loaded.value, Some(Vec::new()));
        assert_eq!(
            loaded.problems,
            [
                "Player \"x\": executables holds \"ab\", shorter than 3 characters, so the row is ignored."
            ]
        );
    }

    #[test]
    fn a_repeated_name_drops_the_later_row_whatever_its_case() {
        let loaded =
            loaded("[[player]]\nname = \"Brave\"\nhidden = true\n[[player]]\nname = \"brave\"\n");
        let rows = loaded.value.unwrap();
        assert_eq!(names(&rows), ["Brave"]);
        assert!(rows[0].hidden);
        assert_eq!(
            loaded.problems,
            ["Player \"brave\": an earlier row has the same name, so the row is ignored."]
        );
    }

    /// A stray space would otherwise make a row that meets no built-in and
    /// does nothing, with no problem to say why.
    #[test]
    fn names_and_patterns_are_trimmed() {
        let loaded =
            loaded("[[player]]\nname = \" Brave \"\nhidden = true\nsmtc_app_ids = [\" brave \"]\n");
        let rows = loaded.value.unwrap();
        assert_eq!(rows[0].name(), "Brave");
        assert_eq!(rows[0].smtc_app_ids(), Some(&["brave".to_owned()][..]));
        assert!(loaded.problems.is_empty());
    }

    #[test]
    fn read_all_keeps_what_load_keeps() {
        let text = "[[player]]\nname = \"mpv\"\nhidden = true\n\
                    [[player]]\nname = \"Odd\"\nexecutables = [\"ab\"]\n";
        assert_eq!(PlayerRow::read_all(text), loaded(text).value.unwrap());
        assert!(PlayerRow::read_all("[[player]\n").is_empty());
    }

    #[test]
    fn an_emptied_list_is_kept_apart_from_one_left_out() {
        let loaded =
            loaded("[[player]]\nname = \"mpv\"\nexecutables = []\nsmtc_app_ids = [\"MPV.EXE\"]\n");
        assert_eq!(
            loaded.value.unwrap(),
            [PlayerRow {
                name: "mpv".to_owned(),
                smtc_app_ids: Some(vec!["mpv.exe".to_owned()]),
                mpris_ids: None,
                executables: Some(Vec::new()),
                hidden: false,
            }]
        );
    }
}
