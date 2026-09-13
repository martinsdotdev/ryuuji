//! The player table: which media players Ryuuji recognises and how each
//! strategy identifies them. The table ships embedded in the binary.

use serde::Deserialize;

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub(crate) struct Player {
    pub name: String,
    /// Substrings of the SMTC source app user model id, lowercased by
    /// [`PlayerTable::parse`] so a match only has to lowercase the app id.
    #[serde(default)]
    pub smtc_app_ids: Vec<String>,
    /// File names of the player's executables, lowercased by
    /// [`PlayerTable::parse`]. The foreground window is tied to a player by
    /// this name alone, so two instances of one player read the same.
    #[serde(default)]
    pub executables: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PlayerTable {
    players: Vec<Player>,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum TableError {
    #[error("players.toml does not parse")]
    Parse(#[source] toml::de::Error),
    #[error("player {name:?} has a {field} entry shorter than 3 characters: {pattern:?}")]
    ShortPattern {
        name: String,
        field: &'static str,
        pattern: String,
    },
    #[error("player name {name:?} appears twice")]
    DuplicateName { name: String },
}

const MIN_PATTERN_LEN: usize = 3;

#[derive(Deserialize)]
struct Document {
    #[serde(default)]
    player: Vec<Player>,
}

impl PlayerTable {
    /// The embedded table; a parse failure is a build defect covered by a test.
    pub(crate) fn builtin() -> PlayerTable {
        PlayerTable::parse(include_str!("players.toml")).expect("embedded players.toml is valid")
    }

    pub(crate) fn parse(text: &str) -> Result<PlayerTable, TableError> {
        let document: Document = toml::from_str(text).map_err(TableError::Parse)?;
        let mut players = document.player;
        for (index, player) in players.iter().enumerate() {
            check_patterns(player)?;
            if players[..index]
                .iter()
                .any(|earlier| earlier.name == player.name)
            {
                return Err(TableError::DuplicateName {
                    name: player.name.clone(),
                });
            }
        }
        players.iter_mut().for_each(lowercase_patterns);
        Ok(PlayerTable { players })
    }

    /// Appends players found at runtime after the table's own entries, so
    /// a built-in entry still wins a match. A name already in the table is
    /// skipped rather than rejected, because discovery lists every installed
    /// browser and the table already names some of them. A short pattern
    /// rejects the whole batch, like [`PlayerTable::parse`]. Returns how
    /// many players were added.
    pub(crate) fn extend(&mut self, players: Vec<Player>) -> Result<usize, TableError> {
        players.iter().try_for_each(check_patterns)?;
        let mut added = 0;
        for mut player in players {
            if self.players.iter().any(|known| known.name == player.name) {
                continue;
            }
            lowercase_patterns(&mut player);
            self.players.push(player);
            added += 1;
        }
        Ok(added)
    }

    #[cfg(test)]
    pub(crate) fn players(&self) -> &[Player] {
        &self.players
    }

    /// Case-insensitive substring match of any pattern against the SMTC app
    /// id; first table entry wins. Substring, because an unpackaged app's id
    /// is a system-assigned string that merely contains the exe name.
    pub(crate) fn match_app_id(&self, app_id: &str) -> Option<&Player> {
        let app_id = app_id.to_lowercase();
        self.players.iter().find(|player| {
            player
                .smtc_app_ids
                .iter()
                .any(|pattern| app_id.contains(pattern))
        })
    }
}

fn check_patterns(player: &Player) -> Result<(), TableError> {
    for (field, patterns) in [
        ("smtc_app_ids", &player.smtc_app_ids),
        ("executables", &player.executables),
    ] {
        if let Some(pattern) = patterns
            .iter()
            .find(|pattern| pattern.chars().count() < MIN_PATTERN_LEN)
        {
            return Err(TableError::ShortPattern {
                name: player.name.clone(),
                field,
                pattern: pattern.clone(),
            });
        }
    }
    Ok(())
}

fn lowercase_patterns(player: &mut Player) {
    for pattern in player
        .smtc_app_ids
        .iter_mut()
        .chain(&mut player.executables)
    {
        *pattern = pattern.to_lowercase();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(table: &PlayerTable) -> Vec<String> {
        table.players().iter().map(|p| p.name.clone()).collect()
    }

    fn player(name: &str, app_id: &str) -> Player {
        Player {
            name: name.to_owned(),
            smtc_app_ids: vec![app_id.to_owned()],
            executables: vec![format!("{}.exe", name.to_lowercase())],
        }
    }

    #[test]
    fn extend_appends_after_the_builtins_and_lowercases() {
        let mut table = PlayerTable::builtin();
        let added = table
            .extend(vec![player("LibreWolf", "83C1C0F3FA8524B1")])
            .unwrap();
        assert_eq!(added, 1);
        assert_eq!(names(&table).last().map(String::as_str), Some("LibreWolf"));
        assert_eq!(
            table
                .match_app_id("83C1C0F3FA8524B1;PrivateBrowsingAUMID")
                .map(|p| p.name.as_str()),
            Some("LibreWolf")
        );
    }

    #[test]
    fn extend_skips_a_name_the_table_already_has_so_the_builtin_wins() {
        let mut table = PlayerTable::builtin();
        let before = names(&table);
        let added = table
            .extend(vec![player("Brave", "0123456789ABCDEF")])
            .unwrap();
        assert_eq!(added, 0);
        assert_eq!(names(&table), before);
        assert_eq!(table.match_app_id("0123456789ABCDEF"), None);
    }

    #[test]
    fn extend_rejects_a_short_pattern_and_adds_nothing() {
        let mut table = PlayerTable::builtin();
        let before = names(&table);
        let result = table.extend(vec![
            player("LibreWolf", "83C1C0F3FA8524B1"),
            player("Odd", "ab"),
        ]);
        assert!(
            matches!(result, Err(TableError::ShortPattern { ref name, .. }) if name == "Odd"),
            "{result:?}"
        );
        assert_eq!(names(&table), before);
    }

    #[test]
    fn builtin_table_parses_with_local_players_then_browsers_in_order() {
        assert_eq!(
            names(&PlayerTable::builtin()),
            ["mpv", "VLC", "MPC-HC", "Brave", "Chromium", "Edge"]
        );
    }

    /// Per-user Chromium installs report the branded id plus a hash of the
    /// Windows account; machine-wide installs report the branded id alone.
    /// The hashed ids are the ones this machine registers.
    #[test]
    fn browser_app_ids_match_their_own_entry_in_either_install_scope() {
        let table = PlayerTable::builtin();
        for (app_id, name) in [
            ("Brave.TOV6AIDIK4HLZU7TATPUSWV77Q", "Brave"),
            ("Brave", "Brave"),
            ("Chromium.TOV6AIDIK4HLZU7TATPUSWV77Q", "Chromium"),
            ("Chromium", "Chromium"),
            ("MSEdge", "Edge"),
        ] {
            assert_eq!(
                table.match_app_id(app_id).map(|p| p.name.as_str()),
                Some(name),
                "{app_id}"
            );
        }
    }

    #[test]
    fn match_is_case_insensitive_and_substring() {
        let table = PlayerTable::builtin();
        for app_id in [
            "MPV.EXE",
            "C:\\tools\\mpv.exe",
            "Microsoft.AutoGenerated.{8B7C2A1E-0000-4000-8000-000000000000}\\mpv.exe",
        ] {
            assert_eq!(
                table.match_app_id(app_id).map(|p| p.name.as_str()),
                Some("mpv")
            );
        }
    }

    #[test]
    fn parse_lowercases_patterns_so_an_uppercase_entry_still_matches() {
        let table = PlayerTable::parse(
            "[[player]]\nname = \"MPV\"\nsmtc_app_ids = [\"MPV.EXE\"]\nexecutables = [\"MPV.EXE\"]\n",
        )
        .unwrap();
        assert_eq!(table.players()[0].smtc_app_ids, ["mpv.exe"]);
        assert_eq!(table.players()[0].executables, ["mpv.exe"]);
        assert_eq!(
            table
                .match_app_id("C:\\tools\\mpv.exe")
                .map(|p| p.name.as_str()),
            Some("MPV")
        );
    }

    #[test]
    fn every_builtin_player_names_an_executable() {
        for player in PlayerTable::builtin().players() {
            assert!(!player.executables.is_empty(), "{}", player.name);
        }
    }

    #[test]
    fn unknown_app_id_matches_nothing() {
        assert_eq!(PlayerTable::builtin().match_app_id("Spotify.exe"), None);
    }

    #[test]
    fn pattern_shorter_than_three_chars_is_rejected_and_names_the_field() {
        for field in ["smtc_app_ids", "executables"] {
            let result =
                PlayerTable::parse(&format!("[[player]]\nname = \"x\"\n{field} = [\"ab\"]\n"));
            assert!(
                matches!(
                    result,
                    Err(TableError::ShortPattern { ref name, field: got, ref pattern })
                        if name == "x" && got == field && pattern == "ab"
                ),
                "{field}: {result:?}"
            );
        }
    }

    #[test]
    fn duplicate_player_name_is_rejected() {
        let text = "[[player]]\nname = \"mpv\"\nsmtc_app_ids = [\"mpv.exe\"]\n\
                    [[player]]\nname = \"mpv\"\nsmtc_app_ids = [\"mpv2.exe\"]\n";
        assert!(matches!(
            PlayerTable::parse(text),
            Err(TableError::DuplicateName { ref name }) if name == "mpv"
        ));
    }

    #[test]
    fn missing_id_lists_are_empty() {
        let table = PlayerTable::parse("[[player]]\nname = \"bare\"\n").unwrap();
        assert_eq!(
            table.players(),
            [Player {
                name: "bare".to_owned(),
                smtc_app_ids: Vec::new(),
                executables: Vec::new(),
            }]
        );
        assert_eq!(table.match_app_id("bare"), None);
    }
}
