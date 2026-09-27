//! Every WoW game folder under the install root (`_retail_`, `_classic_beta_`, …) and which
//! game wrote the GoldCap files in it. The folder name is Blizzard's to change — the Forever
//! launch folder is not known — so a folder is Forever only by the passport the addon stamps
//! into `GoldCapDB.client` (docs/companion/AGENTS.md "Client passport").

use crate::config::Config;
use crate::forever::{is_forever_interface, read_passport};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GameKind {
    Retail,
    Forever,
    Other,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GameInstall {
    pub dir: PathBuf,
    pub folder: String,
    pub kind: GameKind,
    /// The Forever passport's interface, else the first passport's, else None. The configured
    /// retail folder is never opened, so it has none.
    pub interface: Option<i64>,
    pub saved_vars: Vec<PathBuf>,
}

/// `_name_`: Blizzard's own shape for a game folder under the WoW root.
fn is_game_folder(name: &str) -> bool {
    name.len() > 2 && name.starts_with('_') && name.ends_with('_')
}

fn has_game_folder(root: &Path) -> bool {
    std::fs::read_dir(root)
        .map(|entries| {
            entries
                .flatten()
                .any(|e| e.path().is_dir() && is_game_folder(&e.file_name().to_string_lossy()))
        })
        .unwrap_or(false)
}

/// The WoW root: the folder the configured `_retail_` sits in, or else the first detected
/// install base that holds any game folder (Decision E2).
pub fn wow_root(config: &Config) -> Option<PathBuf> {
    let retail = config.wow_retail_path.trim();
    if !retail.is_empty() {
        let path = Path::new(retail);
        if path.file_name().is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case("_retail_")) {
            return path.parent().map(Path::to_path_buf);
        }
    }
    crate::config::wow_base_candidates()
        .into_iter()
        .map(|c| {
            // Some registry values point at `_retail_` itself rather than at the base folder.
            if c.file_name().is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case("_retail_")) {
                c.parent().map(Path::to_path_buf).unwrap_or(c)
            } else {
                c
            }
        })
        .find(|c| has_game_folder(c))
}

fn same_dir(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
}

/// Every game folder under `root` that holds GoldCap SavedVariables, sorted by name.
/// `retail_dir` — the configured `_retail_` — is retail by configuration and none of its files is
/// opened here: retail behaves exactly as it did before this module existed (Decision E3). Any
/// other folder is Forever only when one of its files carries a Forever passport; a retail
/// passport there, or none at all, is Other and is uploaded nowhere.
pub fn discover(root: &Path, retail_dir: Option<&Path>) -> Vec<GameInstall> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut games: Vec<GameInstall> = entries
        .flatten()
        .filter_map(|entry| {
            let dir = entry.path();
            let folder = entry.file_name().to_string_lossy().into_owned();
            if !dir.is_dir() || !is_game_folder(&folder) {
                return None;
            }
            let saved_vars = crate::savedvars::saved_variables_paths(&dir);
            if saved_vars.is_empty() {
                return None;
            }
            if retail_dir.is_some_and(|r| same_dir(r, &dir)) {
                return Some(GameInstall { dir, folder, kind: GameKind::Retail, interface: None, saved_vars });
            }
            let interfaces: Vec<i64> = saved_vars
                .iter()
                .filter_map(|p| std::fs::read_to_string(p).ok())
                .filter_map(|src| read_passport(&src).map(|p| p.interface))
                .collect();
            let forever = interfaces.iter().copied().find(|i| is_forever_interface(*i));
            let kind = if forever.is_some() { GameKind::Forever } else { GameKind::Other };
            Some(GameInstall { dir, folder, kind, interface: forever.or(interfaces.first().copied()), saved_vars })
        })
        .collect();
    games.sort_by(|a, b| a.folder.cmp(&b.folder));
    games
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn root(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("goldcap-games-{label}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn saved(root: &Path, folder: &str, account: &str, lua: &str) {
        let dir = root.join(folder).join("WTF").join("Account").join(account).join("SavedVariables");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("GoldCap.lua"), lua).unwrap();
    }

    const FOREVER: &str = r#"GoldCapDB = { client = { interface = 16001, build = "1.60.1.70009", regionId = 90 } }"#;
    const RETAIL: &str = r#"GoldCapDB = { client = { interface = 120100, build = "12.1.0.69933", regionId = 3 } }"#;
    const UNSTAMPED: &str = r#"GoldCapDB = { ledger = {} }"#;

    #[test]
    fn a_retail_only_machine_lists_retail_alone_and_never_opens_its_files() {
        let r = root("retail-only");
        // A file the new code must never open: it is not even valid Lua.
        saved(&r, "_retail_", "A", "this is not lua {{{");
        let games = discover(&r, Some(&r.join("_retail_")));
        assert_eq!(games.len(), 1);
        assert_eq!(games[0].kind, GameKind::Retail);
        assert_eq!(games[0].folder, "_retail_");
        assert_eq!(games[0].interface, None);
        fs::remove_dir_all(&r).ok();
    }

    #[test]
    fn retail_and_forever_side_by_side() {
        let r = root("both");
        saved(&r, "_retail_", "A", RETAIL);
        saved(&r, "_classic_beta_", "A", FOREVER);
        let games = discover(&r, Some(&r.join("_retail_")));
        let kinds: Vec<(&str, GameKind, Option<i64>)> = games.iter().map(|g| (g.folder.as_str(), g.kind, g.interface)).collect();
        assert_eq!(kinds, vec![("_classic_beta_", GameKind::Forever, Some(16001)), ("_retail_", GameKind::Retail, None)]);
        fs::remove_dir_all(&r).ok();
    }

    #[test]
    fn the_folder_name_never_decides_only_the_passport() {
        let r = root("names");
        saved(&r, "_forever_", "A", FOREVER); // a launch folder name nobody knows yet
        saved(&r, "_ptr_", "A", RETAIL); // a retail passport outside the configured _retail_
        saved(&r, "_classic_era_", "A", UNSTAMPED); // no passport at all
        let games = discover(&r, None);
        let kinds: Vec<(&str, GameKind)> = games.iter().map(|g| (g.folder.as_str(), g.kind)).collect();
        assert_eq!(kinds, vec![("_classic_era_", GameKind::Other), ("_forever_", GameKind::Forever), ("_ptr_", GameKind::Other)]);
        fs::remove_dir_all(&r).ok();
    }

    #[test]
    fn one_forever_account_is_enough_and_folders_without_goldcap_are_skipped() {
        let r = root("accounts");
        saved(&r, "_classic_beta_", "A", UNSTAMPED);
        saved(&r, "_classic_beta_", "B", FOREVER);
        fs::create_dir_all(r.join("_anniversary_").join("WTF")).unwrap(); // no GoldCap.lua anywhere
        fs::create_dir_all(r.join("Data")).unwrap(); // not a game folder
        let games = discover(&r, None);
        assert_eq!(games.len(), 1);
        assert_eq!(games[0].kind, GameKind::Forever);
        assert_eq!(games[0].saved_vars.len(), 2);
        fs::remove_dir_all(&r).ok();
    }

    #[test]
    fn the_root_is_the_folder_the_configured_retail_sits_in() {
        let r = root("config");
        fs::create_dir_all(r.join("_retail_")).unwrap();
        let config = Config { wow_retail_path: r.join("_retail_").to_string_lossy().into_owned(), ..Config::default() };
        assert_eq!(wow_root(&config), Some(r.clone()));
        fs::remove_dir_all(&r).ok();
    }

    #[test]
    fn is_game_folder_is_blizzards_underscore_shape() {
        for ok in ["_retail_", "_classic_beta_", "_ptr_", "_x_"] {
            assert!(is_game_folder(ok), "{ok}");
        }
        for no in ["_", "__", "retail", "_retail", "Data", "Interface"] {
            assert!(!is_game_folder(no), "{no}");
        }
    }
}
