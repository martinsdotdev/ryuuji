//! Gecko browsers found at runtime. Firefox and its forks publish a media
//! session under an app user model id that is a hash of the install
//! directory, different on every machine, so they cannot be listed in
//! `players.toml`. This module reads the installed browsers from the
//! registry and computes the id the same way the browser does.
//!
//! The rule is `GenerateAppUserModelID` in Firefox's
//! `widget/windows/WinTaskbar.cpp`: a value under
//! `Software\Mozilla\<app>\TaskBarIDs` named by the install directory if
//! one exists, else CityHash64 (Version 1) of the directory's UTF-16 bytes
//! as sixteen uppercase hex digits. A private window appends
//! `;PrivateBrowsingAUMID`, which a substring match on the bare hash still
//! catches. The profile-hash variant behind the off-by-default
//! `taskbar.grouping.useprofile` pref is not handled.

use std::path::{Path, PathBuf};

use windows_registry::{CURRENT_USER, Key, LOCAL_MACHINE};

use crate::players::Player;

mod city;
use city::city_hash_64;

const BROWSERS: &str = r"SOFTWARE\Clients\StartMenuInternet";
const MOZILLA: &str = r"SOFTWARE\Mozilla";
const HIVES: [&Key; 2] = [LOCAL_MACHINE, CURRENT_USER];

/// One entry per browser registered in either hive, Gecko or not. A
/// Chromium browser's session carries a branded id, so its hash entry
/// matches nothing; the table skips it anyway when the name is built in.
pub(crate) fn discover() -> Vec<Player> {
    let mut players = Vec::new();
    for hive in HIVES {
        let Ok(root) = hive.open(BROWSERS) else {
            continue;
        };
        let Ok(keys) = root.keys() else {
            continue;
        };
        for key in keys {
            let Ok(entry) = root.open(&key) else {
                continue;
            };
            let Ok(command) = entry
                .open(r"shell\open\command")
                .and_then(|command| command.get_string(""))
            else {
                continue;
            };
            let Some(exe) = executable(&command) else {
                continue;
            };
            let (Some(dir), Some(file)) = (exe.parent(), exe.file_name()) else {
                continue;
            };
            let name = entry
                .get_string("")
                .ok()
                .filter(|name| !name.is_empty())
                .unwrap_or(key);
            let canonical = canonical(dir);
            let id = taskbar_id(dir, &canonical).unwrap_or_else(|| hash_of(&canonical));
            players.push(Player {
                name,
                smtc_app_ids: vec![id],
                mpris_ids: Vec::new(),
                executables: vec![file.to_string_lossy().into_owned()],
            });
        }
    }
    players
}

/// Whether an unmatched app id has the shape of an install hash, so a
/// fresh discovery is worth a try.
pub(crate) fn is_install_hash(app_id: &str) -> bool {
    let hash = app_id.split(';').next().unwrap_or_default();
    hash.len() == 16 && hash.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The id Firefox computes for an install directory: CityHash64 of the
/// canonical path's UTF-16 bytes, no trailing separator, uppercase hex.
fn hash_of(canonical_dir: &str) -> String {
    let bytes: Vec<u8> = canonical_dir
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    format!("{:016X}", city_hash_64(&bytes))
}

/// The on-disk spelling of the path without the `\\?\` prefix
/// `canonicalize` adds on Windows; the path as given if it cannot be
/// resolved.
fn canonical(dir: &Path) -> String {
    let resolved = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    let text = resolved.to_string_lossy();
    text.strip_prefix(r"\\?\")
        .unwrap_or(&text)
        .trim_end_matches('\\')
        .to_owned()
}

/// An id pinned in the registry for this install directory, checked
/// before the hash because Firefox checks it first.
fn taskbar_id(dir: &Path, canonical: &str) -> Option<String> {
    let raw = dir.to_string_lossy();
    let names = [raw.as_ref(), canonical];
    for hive in HIVES {
        let Ok(root) = hive.open(MOZILLA) else {
            continue;
        };
        let Ok(apps) = root.keys() else {
            continue;
        };
        for app in apps {
            let Ok(ids) = root.open(format!(r"{app}\TaskBarIDs")) else {
                continue;
            };
            if let Some(id) = names.iter().find_map(|name| ids.get_string(name).ok()) {
                return Some(id);
            }
        }
    }
    None
}

/// The executable named by a `shell\open\command` value. Quoted commands
/// end at the closing quote; bare ones end at the first `.exe`, since an
/// unquoted path may contain spaces.
fn executable(command: &str) -> Option<PathBuf> {
    let command = command.trim();
    let path = match command.strip_prefix('"') {
        Some(rest) => rest.split('"').next()?,
        None => {
            let lower = command.to_ascii_lowercase();
            match lower.find(".exe") {
                Some(at) => &command[..at + ".exe".len()],
                None => command.split(' ').next()?,
            }
        }
    };
    (!path.is_empty()).then(|| PathBuf::from(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn install_id(dir: &Path) -> String {
        hash_of(&canonical(dir))
    }

    #[test]
    fn librewolfs_install_directory_gives_its_observed_app_id() {
        let dir = Path::new(r"C:\Program Files\LibreWolf");
        if !dir.is_dir() {
            eprintln!("LibreWolf is not installed here; skipping");
            return;
        }
        assert_eq!(install_id(dir), "83C1C0F3FA8524B1");
        assert_eq!(
            install_id(Path::new(r"c:\program files\librewolf")),
            "83C1C0F3FA8524B1"
        );
    }

    #[test]
    fn install_id_is_stable_and_follows_the_path() {
        let root = tempfile::tempdir().unwrap();
        let one = root.path().join("One");
        let two = root.path().join("Two");
        std::fs::create_dir_all(&one).unwrap();
        std::fs::create_dir_all(&two).unwrap();
        assert_eq!(install_id(&one), install_id(&one));
        assert_ne!(install_id(&one), install_id(&two));
        assert_eq!(install_id(&one).len(), 16);
    }

    #[test]
    fn executable_reads_quoted_bare_and_argumented_commands() {
        for (command, expected) in [
            (
                r#""C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe""#,
                r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
            ),
            (
                r"C:\Program Files\LibreWolf\librewolf.exe",
                r"C:\Program Files\LibreWolf\librewolf.exe",
            ),
            (
                r#""C:\Users\u\AppData\Local\Chromium\Application\chrome.exe" --flag"#,
                r"C:\Users\u\AppData\Local\Chromium\Application\chrome.exe",
            ),
            (
                r"C:\Tools\Waterfox\Waterfox.EXE -osint",
                r"C:\Tools\Waterfox\Waterfox.EXE",
            ),
        ] {
            assert_eq!(
                executable(command),
                Some(PathBuf::from(expected)),
                "{command}"
            );
        }
        assert_eq!(executable(""), None);
        assert_eq!(executable("\"\""), None);
    }

    #[test]
    fn an_install_hash_is_sixteen_hex_digits_with_or_without_the_private_suffix() {
        assert!(is_install_hash("83C1C0F3FA8524B1"));
        assert!(is_install_hash("83C1C0F3FA8524B1;PrivateBrowsingAUMID"));
        assert!(!is_install_hash("Brave.TOV6AIDIK4HLZU7TATPUSWV77Q"));
        assert!(!is_install_hash("MSEdge"));
        assert!(!is_install_hash("83C1C0F3FA8524B"));
    }

    /// Every registered browser on this machine yields one entry with a
    /// name, one id and an executable name.
    #[test]
    fn discovery_lists_registered_browsers_with_well_formed_entries() {
        for player in discover() {
            assert!(!player.name.is_empty());
            assert_eq!(player.smtc_app_ids.len(), 1, "{}", player.name);
            assert!(
                player.executables[0].to_ascii_lowercase().ends_with(".exe"),
                "{}",
                player.name
            );
        }
    }
}
