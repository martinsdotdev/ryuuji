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
    /// What the player calls itself on the session bus: the tail of its
    /// `org.mpris.MediaPlayer2.` name with any `.instance…` suffix cut, or
    /// its `DesktopEntry`. Lowercased by [`PlayerTable::parse`] and matched
    /// whole.
    #[serde(default)]
    pub mpris_ids: Vec<String>,
    /// File names of the player's executables, lowercased by
    /// [`PlayerTable::parse`]. The foreground window is tied to a player by
    /// this name alone, so two instances of one player read the same.
    #[serde(default)]
    pub executables: Vec<String>,
}

impl Player {
    /// Whether a player found at runtime is this row. The name, or a shared
    /// executable: the registry says "Mozilla Firefox" where the row says
    /// "Firefox", and both say `firefox.exe`.
    fn is_same(&self, other: &Player) -> bool {
        self.name == other.name
            || self
                .executables
                .iter()
                .any(|exe| other.executables.contains(exe))
    }

    /// Fills the pattern lists this row left empty from `other`. A list the
    /// row already wrote is never overridden. Returns whether anything was
    /// filled.
    fn fill_from(&mut self, other: Player) -> bool {
        let mut filled = false;
        for (mine, theirs) in [
            (&mut self.smtc_app_ids, other.smtc_app_ids),
            (&mut self.mpris_ids, other.mpris_ids),
            (&mut self.executables, other.executables),
        ] {
            if mine.is_empty() && !theirs.is_empty() {
                *mine = theirs;
                filled = true;
            }
        }
        filled
    }
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
const MPRIS_PREFIX: &str = "org.mpris.MediaPlayer2.";

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

    /// Adds players found at runtime. One the table does not know is
    /// appended after the built-in entries, so a built-in entry still wins a
    /// match. One it knows, by name or by a shared executable, fills only the
    /// pattern lists its row left empty: a row written for one platform is
    /// completed by the other's discovery, never shadowed, and a built-in
    /// pattern list is never overridden. A short pattern rejects the whole
    /// batch, like [`PlayerTable::parse`]. Returns how many rows were
    /// appended or filled.
    pub(crate) fn extend(&mut self, players: Vec<Player>) -> Result<usize, TableError> {
        players.iter().try_for_each(check_patterns)?;
        let mut changed = 0;
        for mut player in players {
            lowercase_patterns(&mut player);
            match self.players.iter_mut().find(|known| known.is_same(&player)) {
                Some(known) => changed += usize::from(known.fill_from(player)),
                None => {
                    self.players.push(player);
                    changed += 1;
                }
            }
        }
        Ok(changed)
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

    /// The player an MPRIS session belongs to. The bus name's tail after the
    /// well-known prefix, with any `.instance…` suffix cut, is compared whole
    /// and case-insensitively; the desktop entry answers the same way. Whole,
    /// not substring: the tail is the app's own short name, and `firefox`
    /// must not claim `firefox-esr-wrapper`. First table entry wins.
    #[cfg_attr(windows, allow(dead_code))]
    pub(crate) fn match_mpris(
        &self,
        bus_name: &str,
        desktop_entry: Option<&str>,
    ) -> Option<&Player> {
        let tail = bus_name.strip_prefix(MPRIS_PREFIX)?;
        let tail = tail
            .find(".instance")
            .map_or(tail, |at| &tail[..at])
            .to_lowercase();
        let entry = desktop_entry.map(str::to_lowercase);
        self.players.iter().find(|player| {
            player
                .mpris_ids
                .iter()
                .any(|id| *id == tail || entry.as_deref() == Some(id))
        })
    }
}

fn check_patterns(player: &Player) -> Result<(), TableError> {
    for (field, patterns) in [
        ("smtc_app_ids", &player.smtc_app_ids),
        ("mpris_ids", &player.mpris_ids),
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
        .chain(&mut player.mpris_ids)
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
            mpris_ids: Vec::new(),
            executables: vec![format!("{}.exe", name.to_lowercase())],
        }
    }

    fn table(text: &str) -> PlayerTable {
        PlayerTable::parse(text).unwrap()
    }

    const FIREFOX_ROW: &str = "[[player]]\nname = \"Firefox\"\nmpris_ids = [\"firefox\"]\nexecutables = [\"firefox.exe\"]\n";

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
    fn extend_never_overrides_a_pattern_list_a_known_row_already_wrote() {
        let mut table = PlayerTable::builtin();
        let before = names(&table);
        let changed = table
            .extend(vec![player("Brave", "0123456789ABCDEF")])
            .unwrap();
        assert_eq!(changed, 0);
        assert_eq!(names(&table), before);
        assert_eq!(table.match_app_id("0123456789ABCDEF"), None);
        assert_eq!(
            table.match_app_id("Brave").map(|p| p.name.as_str()),
            Some("Brave")
        );
    }

    /// The registry calls it "Mozilla Firefox" and the row calls it
    /// "Firefox"; the executable is what ties them, and the discovered
    /// install hash lands in the list the row left empty.
    #[test]
    fn extend_completes_a_row_written_for_another_platform() {
        let mut table = table(FIREFOX_ROW);
        let discovered = Player {
            name: "Mozilla Firefox".to_owned(),
            smtc_app_ids: vec!["D52277D1BA334E98".to_owned()],
            mpris_ids: Vec::new(),
            executables: vec!["firefox.exe".to_owned()],
        };
        assert_eq!(table.extend(vec![discovered]).unwrap(), 1);
        assert_eq!(names(&table), ["Firefox"]);
        assert_eq!(
            table
                .match_app_id("D52277D1BA334E98")
                .map(|p| p.name.as_str()),
            Some("Firefox")
        );
        assert_eq!(
            table
                .match_mpris("org.mpris.MediaPlayer2.firefox.instance42", None)
                .map(|p| p.name.as_str()),
            Some("Firefox")
        );
    }

    #[test]
    fn extend_fills_by_name_when_the_executables_differ() {
        let mut table = table("[[player]]\nname = \"Waterfox\"\nmpris_ids = [\"waterfox\"]\n");
        let discovered = Player {
            name: "Waterfox".to_owned(),
            smtc_app_ids: vec!["0123456789ABCDEF".to_owned()],
            mpris_ids: Vec::new(),
            executables: vec!["Waterfox.EXE".to_owned()],
        };
        assert_eq!(table.extend(vec![discovered]).unwrap(), 1);
        assert_eq!(names(&table), ["Waterfox"]);
        assert_eq!(table.players()[0].executables, ["waterfox.exe"]);
        assert_eq!(
            table
                .match_app_id("0123456789abcdef")
                .map(|p| p.name.as_str()),
            Some("Waterfox")
        );
    }

    #[test]
    fn match_mpris_cuts_the_instance_suffix_and_ignores_case() {
        let table = table(FIREFOX_ROW);
        for bus_name in [
            "org.mpris.MediaPlayer2.firefox",
            "org.mpris.MediaPlayer2.firefox.instance1234",
            "org.mpris.MediaPlayer2.Firefox.instance_1_2",
        ] {
            assert_eq!(
                table.match_mpris(bus_name, None).map(|p| p.name.as_str()),
                Some("Firefox"),
                "{bus_name}"
            );
        }
    }

    #[test]
    fn match_mpris_is_whole_not_substring_and_needs_the_prefix() {
        let table = table(FIREFOX_ROW);
        assert_eq!(
            table.match_mpris("org.mpris.MediaPlayer2.firefox-esr-wrapper", None),
            None
        );
        assert_eq!(table.match_mpris("firefox", None), None);
        assert_eq!(table.match_mpris("org.mpris.MediaPlayer2.mpv", None), None);
    }

    #[test]
    fn match_mpris_falls_back_to_the_desktop_entry() {
        let table = table(FIREFOX_ROW);
        assert_eq!(
            table
                .match_mpris("org.mpris.MediaPlayer2.io.example.Wrapper", Some("Firefox"))
                .map(|p| p.name.as_str()),
            Some("Firefox")
        );
        assert_eq!(
            table.match_mpris("org.mpris.MediaPlayer2.io.example.Wrapper", Some("mpv")),
            None
        );
    }

    #[test]
    fn builtin_mpris_ids_match_their_players() {
        let table = PlayerTable::builtin();
        for (bus_name, name) in [
            ("org.mpris.MediaPlayer2.mpv", "mpv"),
            ("org.mpris.MediaPlayer2.vlc", "VLC"),
            ("org.mpris.MediaPlayer2.chromium.instance7", "Chromium"),
            ("org.mpris.MediaPlayer2.firefox.instance7", "Firefox"),
        ] {
            assert_eq!(
                table.match_mpris(bus_name, None).map(|p| p.name.as_str()),
                Some(name),
                "{bus_name}"
            );
        }
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
            [
                "mpv", "VLC", "MPC-HC", "Brave", "Chromium", "Edge", "Firefox"
            ]
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
        for field in ["smtc_app_ids", "mpris_ids", "executables"] {
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
                mpris_ids: Vec::new(),
                executables: Vec::new(),
            }]
        );
        assert_eq!(table.match_app_id("bare"), None);
    }
}
