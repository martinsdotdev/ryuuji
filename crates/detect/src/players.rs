//! The player table: which media players Ryuuji recognises and how each
//! strategy identifies them. The built-in rows ship embedded in the binary,
//! and the rows a person wrote in `players.toml` are laid over them.

use ryuuji_core::{PlayerRow, too_short};
use serde::Deserialize;

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
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
    /// A person turned the player off. Its sessions are still matched, so
    /// discovery finds the row and does not add the player again, but none
    /// of them is watched.
    #[serde(default)]
    pub hidden: bool,
}

impl Player {
    /// Takes the lists `row` wrote, emptied ones included, and whether it is
    /// hidden. The name stays, because history stores it.
    fn apply(&mut self, row: &PlayerRow) {
        for (mine, theirs) in [
            (&mut self.smtc_app_ids, row.smtc_app_ids()),
            (&mut self.mpris_ids, row.mpris_ids()),
            (&mut self.executables, row.executables()),
        ] {
            if let Some(theirs) = theirs {
                *mine = theirs.to_vec();
            }
        }
        self.hidden = row.hidden();
    }

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

/// The players Ryuuji recognises, in two layers, as Windows Terminal keeps
/// its profiles. The base holds the built-in rows and what discovery found,
/// and the person's rows lie over it. Discovery reads and writes the base
/// alone, so a person's row can neither mislead it nor be refilled by it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PlayerTable {
    base: Vec<Player>,
    rows: Vec<PlayerRow>,
    /// The base with the rows laid over it, which every match reads.
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

    /// The built-in table with a person's rows laid over it.
    pub(crate) fn with_rows(rows: Vec<PlayerRow>) -> PlayerTable {
        let mut table = PlayerTable::builtin();
        table.rows = rows;
        table.lay_rows();
        table
    }

    /// Lays `rows` over the base in place of the rows it had. Returns
    /// whether they differed; the same rows again change nothing.
    pub(crate) fn set_rows(&mut self, rows: Vec<PlayerRow>) -> bool {
        if rows == self.rows {
            return false;
        }
        self.rows = rows;
        self.lay_rows();
        true
    }

    fn of(base: Vec<Player>) -> PlayerTable {
        PlayerTable {
            players: base.clone(),
            base,
            rows: Vec::new(),
        }
    }

    /// Lays the rows over the base. A row named like a player in the base,
    /// whatever the case, changes that player in place and keeps its name,
    /// which history stores. Any other row goes ahead of the base in the
    /// order written, so it wins an id it shares with one. The core has
    /// already checked, trimmed and lowercased the rows.
    fn lay_rows(&mut self) {
        let mut players = self.base.clone();
        let mut added = Vec::new();
        for row in &self.rows {
            let name = row.name().to_lowercase();
            match players
                .iter_mut()
                .find(|player| player.name.to_lowercase() == name)
            {
                Some(player) => player.apply(row),
                None => {
                    let mut player = Player {
                        name: row.name().to_owned(),
                        ..Player::default()
                    };
                    player.apply(row);
                    added.push(player);
                }
            }
        }
        added.append(&mut players);
        self.players = added;
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
        Ok(PlayerTable::of(players))
    }

    /// Adds players found at runtime to the base, then lays the person's
    /// rows over it again. One the base does not know is appended after the
    /// built-in entries, so a built-in entry still wins a match. One it
    /// knows, by name or by a shared executable, fills only the pattern lists
    /// its row left empty: a row written for one platform is completed by the
    /// other's discovery, never shadowed, and a built-in pattern list is
    /// never overridden. A short pattern rejects the whole batch, like
    /// [`PlayerTable::parse`]. Returns how many rows were appended or filled.
    pub(crate) fn extend(&mut self, players: Vec<Player>) -> Result<usize, TableError> {
        players.iter().try_for_each(check_patterns)?;
        let mut changed = 0;
        for mut player in players {
            lowercase_patterns(&mut player);
            match self.base.iter_mut().find(|known| known.is_same(&player)) {
                Some(known) => changed += usize::from(known.fill_from(player)),
                None => {
                    self.base.push(player);
                    changed += 1;
                }
            }
        }
        if changed > 0 {
            self.lay_rows();
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
        if let Some(pattern) = patterns.iter().find(|pattern| too_short(pattern)) {
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
            hidden: false,
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
            hidden: false,
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
            hidden: false,
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

    /// The rows as the core reads them from `players.toml`.
    fn with(text: &str) -> PlayerTable {
        PlayerTable::with_rows(PlayerRow::read_all(text))
    }

    fn named<'a>(table: &'a PlayerTable, name: &str) -> &'a Player {
        table.players().iter().find(|p| p.name == name).unwrap()
    }

    /// What the registry reports for release Firefox.
    fn mozilla_firefox() -> Player {
        Player {
            name: "Mozilla Firefox".to_owned(),
            smtc_app_ids: vec!["D52277D1BA334E98".to_owned()],
            mpris_ids: Vec::new(),
            executables: vec!["firefox.exe".to_owned()],
            hidden: false,
        }
    }

    #[test]
    fn with_no_rows_is_the_builtin_table() {
        assert_eq!(PlayerTable::with_rows(Vec::new()), PlayerTable::builtin());
    }

    #[test]
    fn a_row_named_like_a_builtin_replaces_only_the_lists_it_wrote() {
        let table = with("[[player]]\nname = \"mpv\"\nexecutables = []\n");
        assert_eq!(names(&table), names(&PlayerTable::builtin()));
        let mpv = named(&table, "mpv");
        assert!(mpv.executables.is_empty());
        assert_eq!(mpv.smtc_app_ids, ["mpv.exe"]);
        assert_eq!(mpv.mpris_ids, ["mpv"]);
        assert!(!mpv.hidden);
    }

    /// History stores the player's name, so a row written in another case
    /// changes the built-in without renaming it.
    #[test]
    fn a_row_meets_a_builtin_whatever_the_case_and_keeps_its_name() {
        let table = with("[[player]]\nname = \"brave\"\nhidden = true\n");
        assert_eq!(names(&table), names(&PlayerTable::builtin()));
        let brave = table
            .match_app_id("Brave.TOV6AIDIK4HLZU7TATPUSWV77Q")
            .unwrap();
        assert_eq!(brave.name, "Brave");
        assert!(brave.hidden);
    }

    #[test]
    fn a_new_row_goes_ahead_of_the_builtins_and_wins_a_shared_id() {
        let table = with(
            "[[player]]\nname = \"Wrapper\"\nsmtc_app_ids = [\"mpv.exe\"]\n\
             [[player]]\nname = \"PotPlayer\"\nsmtc_app_ids = [\"potplayer\"]\n",
        );
        assert_eq!(names(&table)[..3], ["Wrapper", "PotPlayer", "mpv"]);
        assert_eq!(
            table
                .match_app_id("C:\\tools\\mpv.exe")
                .map(|p| p.name.as_str()),
            Some("Wrapper")
        );
    }

    /// A hidden row stays in the table, so discovery ties the install hash
    /// to it rather than appending "Mozilla Firefox" as a player of its own.
    #[test]
    fn discovery_fills_a_hidden_row_and_it_stays_hidden() {
        let mut table = with("[[player]]\nname = \"Firefox\"\nhidden = true\n");
        assert_eq!(table.extend(vec![mozilla_firefox()]).unwrap(), 1);
        assert_eq!(names(&table), names(&PlayerTable::builtin()));
        let firefox = table.match_app_id("D52277D1BA334E98").unwrap();
        assert_eq!(firefox.name, "Firefox");
        assert!(firefox.hidden);
    }

    /// Discovery ties to the built-in by its executable, which the person's
    /// row does not take away, and the row's lists still win over what
    /// discovery fills.
    #[test]
    fn emptied_lists_stay_empty_and_keep_the_tie_to_discovery() {
        let mut table =
            with("[[player]]\nname = \"Firefox\"\nexecutables = []\nsmtc_app_ids = []\n");
        assert_eq!(table.extend(vec![mozilla_firefox()]).unwrap(), 1);
        assert_eq!(names(&table), names(&PlayerTable::builtin()));
        let firefox = named(&table, "Firefox");
        assert!(firefox.executables.is_empty());
        assert!(firefox.smtc_app_ids.is_empty());
        assert_eq!(table.match_app_id("D52277D1BA334E98"), None);
    }

    /// A Gecko fork is not built in, so the person's row for it only meets
    /// the player once discovery has added it, and meets it whatever the case.
    #[test]
    fn a_row_for_a_discovered_browser_meets_it_whatever_the_case() {
        let mut table = with("[[player]]\nname = \"librewolf\"\nhidden = true\n");
        let librewolf = Player {
            name: "LibreWolf".to_owned(),
            smtc_app_ids: vec!["83C1C0F3FA8524B1".to_owned()],
            mpris_ids: Vec::new(),
            executables: vec!["librewolf.exe".to_owned()],
            hidden: false,
        };
        assert_eq!(table.extend(vec![librewolf]).unwrap(), 1);
        let found = table.match_app_id("83C1C0F3FA8524B1").unwrap();
        assert_eq!(found.name, "LibreWolf");
        assert!(found.hidden);
        assert_eq!(
            names(&table)
                .iter()
                .filter(|name| name.eq_ignore_ascii_case("librewolf"))
                .count(),
            1
        );
    }

    /// A save lays new rows over the same base, so a browser discovery found
    /// stays found and nothing has to scan the registry again.
    #[test]
    fn new_rows_keep_what_discovery_found_and_the_same_rows_change_nothing() {
        let mut table = with("[[player]]\nname = \"mpv\"\nhidden = true\n");
        assert_eq!(table.extend(vec![mozilla_firefox()]).unwrap(), 1);
        assert!(!table.set_rows(PlayerRow::read_all(
            "[[player]]\nname = \"mpv\"\nhidden = true\n"
        )));
        assert!(table.set_rows(PlayerRow::read_all(
            "[[player]]\nname = \"Firefox\"\nhidden = true\n"
        )));
        let firefox = table.match_app_id("D52277D1BA334E98").unwrap();
        assert!(firefox.hidden);
        assert!(!named(&table, "mpv").hidden);
    }

    /// Discovery only sees the base, so a person's row that names the same
    /// executable cannot take release Firefox's install hash.
    #[test]
    fn a_row_sharing_an_executable_does_not_take_a_discovered_hash() {
        let mut table = with(
            "[[player]]\nname = \"Firefox Nightly\"\nexecutables = [\"firefox.exe\"]\n\
             smtc_app_ids = [\"6F193CCC56814779\"]\n",
        );
        assert_eq!(table.extend(vec![mozilla_firefox()]).unwrap(), 1);
        assert_eq!(
            table
                .match_app_id("D52277D1BA334E98")
                .map(|p| p.name.as_str()),
            Some("Firefox")
        );
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
                hidden: false,
            }]
        );
        assert_eq!(table.match_app_id("bare"), None);
    }
}
