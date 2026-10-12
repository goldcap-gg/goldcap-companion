//! Reads the two pieces of the WoW client's own on-disk state the Settings
//! window can prefill from: the login region (`SET portal "EU"` in
//! `WTF/Config.wtf`) and the realm names the player has characters on
//! (the directory names under `WTF/Account/<ACCOUNT>/`). Realm folder names
//! are display names in the client's locale ("Tarren Mill", "Гордунни") —
//! mapping them to a connected-realm slug is the API's job
//! (`GET /v1/addon/resolve-realm`), not ours.

use std::fs;
use std::path::Path;
use std::time::SystemTime;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GameSettings {
    /// Lowercased login portal ("eu"/"us"/...) when Config.wtf declares one.
    pub region: Option<String>,
    /// Realm display names found under WTF/Account/*/, most recently played
    /// first (see `last_played`), deduped across accounts.
    pub realm_names: Vec<String>,
}

pub fn read_game_settings(wow_retail_path: &Path) -> GameSettings {
    GameSettings {
        region: read_portal(wow_retail_path),
        realm_names: list_realm_names(wow_retail_path),
    }
}

fn read_portal(wow_retail_path: &Path) -> Option<String> {
    let config = fs::read_to_string(wow_retail_path.join("WTF").join("Config.wtf")).ok()?;
    parse_portal(&config)
}

/// Extracts the login region from Config.wtf contents: `SET portal "EU"`.
pub fn parse_portal(config_wtf: &str) -> Option<String> {
    for line in config_wtf.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("SET ") else {
            continue;
        };
        let mut parts = rest.splitn(2, char::is_whitespace);
        if parts.next() != Some("portal") {
            continue;
        }
        let value = parts.next()?.trim().trim_matches('"').trim();
        if value.is_empty() {
            return None;
        }
        return Some(value.to_ascii_lowercase());
    }
    None
}

/// Directory names under `WTF/Account/<ACCOUNT>/` are realm display names;
/// `SavedVariables` is the one non-realm directory living at that level.
/// Sorted by `last_played`, newest first: the realm the player logged into
/// most recently comes out on top.
pub fn list_realm_names(wow_retail_path: &Path) -> Vec<String> {
    let account_root = wow_retail_path.join("WTF").join("Account");
    let Ok(accounts) = fs::read_dir(&account_root) else {
        return Vec::new();
    };

    let mut found: Vec<(String, SystemTime)> = Vec::new();
    for account in accounts.flatten() {
        if !account.path().is_dir() {
            continue;
        }
        let Ok(realms) = fs::read_dir(account.path()) else {
            continue;
        };
        for realm in realms.flatten() {
            let path = realm.path();
            if !path.is_dir() {
                continue;
            }
            let name = realm.file_name().to_string_lossy().into_owned();
            if name == "SavedVariables" {
                continue;
            }
            found.push((name, last_played(&path)));
        }
    }

    found.sort_by(|a, b| b.1.cmp(&a.1));
    let mut names: Vec<String> = Vec::with_capacity(found.len());
    for (name, _) in found {
        if !names.contains(&name) {
            names.push(name);
        }
    }
    names
}

/// When the player was last on a realm: the newest file under its folder (a
/// character's `config-cache.wtf`, or an addon's SavedVariables), not the
/// folder's own date. WoW writes those files on every logout and /reload but
/// changes the realm folder itself only when a character is created there, so
/// the folder's date points at the realm with the newest character, not the
/// one played last. Three levels reach `<Realm>/<Character>/SavedVariables/*`.
fn last_played(realm: &Path) -> SystemTime {
    newest_modified(realm, 3)
}

fn newest_modified(path: &Path, depth: u8) -> SystemTime {
    let mut newest = fs::metadata(path)
        .and_then(|m| m.modified())
        .unwrap_or(SystemTime::UNIX_EPOCH);
    if depth == 0 {
        return newest;
    }
    let Ok(entries) = fs::read_dir(path) else {
        return newest;
    };
    for entry in entries.flatten() {
        let modified = match entry.file_type() {
            Ok(kind) if kind.is_dir() => newest_modified(&entry.path(), depth - 1),
            Ok(_) => entry
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH),
            Err(_) => continue,
        };
        newest = newest.max(modified);
    }
    newest
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "goldcap-companion-wtf-{label}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn parses_the_portal_cvar() {
        let config = "SET textLocale \"enGB\"\nSET portal \"EU\"\nSET agentUID \"wow_engb\"\n";
        assert_eq!(parse_portal(config), Some("eu".to_string()));
    }

    #[test]
    fn parses_portal_without_quotes_and_with_padding() {
        assert_eq!(parse_portal("  SET portal US  "), Some("us".to_string()));
    }

    #[test]
    fn portal_absent_or_empty_yields_none() {
        assert_eq!(parse_portal("SET textLocale \"enUS\"\n"), None);
        assert_eq!(parse_portal("SET portal \"\"\n"), None);
        assert_eq!(parse_portal(""), None);
        // `portalLite` or similar must not match the `portal` cvar.
        assert_eq!(parse_portal("SET portalLite \"1\"\n"), None);
    }

    #[test]
    fn lists_realm_dirs_excluding_savedvariables_and_dedupes_across_accounts() {
        let dir = temp_dir("realms");
        let account_root = dir.join("WTF").join("Account");
        for (account, realms) in [
            (
                "ACCOUNT1",
                vec!["Tarren Mill", "SavedVariables", "Silvermoon"],
            ),
            ("ACCOUNT2", vec!["Tarren Mill", "SavedVariables"]),
        ] {
            for realm in realms {
                fs::create_dir_all(account_root.join(account).join(realm)).unwrap();
            }
        }

        let names = list_realm_names(&dir);
        assert_eq!(
            names.len(),
            2,
            "SavedVariables excluded, Tarren Mill deduped: {names:?}"
        );
        assert!(names.contains(&"Tarren Mill".to_string()));
        assert!(names.contains(&"Silvermoon".to_string()));
        assert!(!names.contains(&"SavedVariables".to_string()));

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_realm_played_last_comes_first_even_when_another_realm_folder_is_newer() {
        // WoW dates a realm folder when a character is created on it; playing
        // only rewrites the files inside. Draenor is the older folder but was
        // played last, so it comes first.
        let dir = temp_dir("last-played");
        let account = dir.join("WTF").join("Account").join("ACC");
        let saved = account.join("Draenor").join("Meolis").join("SavedVariables");
        fs::create_dir_all(&saved).unwrap();
        let file = saved.join("GoldCap.lua");
        fs::write(&file, "GoldCapDB = {}").unwrap();
        // So Tarren Mill's folder is strictly the newer one on any filesystem.
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::create_dir_all(account.join("Tarren Mill").join("Kovaqq")).unwrap();
        let logged_out = SystemTime::now() + std::time::Duration::from_secs(3600);
        fs::File::options()
            .write(true)
            .open(&file)
            .unwrap()
            .set_modified(logged_out)
            .unwrap();

        assert_eq!(
            list_realm_names(&dir),
            vec!["Draenor".to_string(), "Tarren Mill".to_string()]
        );

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_wtf_tree_yields_empty_settings() {
        let dir = temp_dir("empty");
        fs::create_dir_all(&dir).unwrap();

        let settings = read_game_settings(&dir);
        assert_eq!(settings.region, None);
        assert!(settings.realm_names.is_empty());

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn reads_region_and_realms_end_to_end() {
        let dir = temp_dir("full");
        fs::create_dir_all(dir.join("WTF")).unwrap();
        fs::write(dir.join("WTF").join("Config.wtf"), "SET portal \"EU\"\n").unwrap();
        fs::create_dir_all(
            dir.join("WTF")
                .join("Account")
                .join("ACC")
                .join("Tarren Mill"),
        )
        .unwrap();

        let settings = read_game_settings(&dir);
        assert_eq!(settings.region, Some("eu".to_string()));
        assert_eq!(settings.realm_names, vec!["Tarren Mill".to_string()]);

        fs::remove_dir_all(&dir).ok();
    }
}
