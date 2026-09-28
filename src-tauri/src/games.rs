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

pub fn has_game_folder(root: &Path) -> bool {
    std::fs::read_dir(root)
        .map(|entries| {
            entries
                .flatten()
                .any(|e| e.path().is_dir() && is_game_folder(&e.file_name().to_string_lossy()))
        })
        .unwrap_or(false)
}

/// The WoW root: the player's own `wow_root_path` when the wizard (or Settings) has set one,
/// else the folder the configured `_retail_` sits in, else the first detected install base that
/// holds any game folder (Decision E2).
pub fn wow_root(config: &Config) -> Option<PathBuf> {
    let explicit = config.wow_root_path.trim();
    if !explicit.is_empty() {
        return Some(PathBuf::from(explicit));
    }
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

/// Best-effort auto-detect of the WoW base install folder, for the wizard's "Which WoW do you
/// play?" step and Settings' "Detect" button — the same candidates `wow_root` falls back to when
/// nothing is configured yet, but usable before any `Config` exists. Empty string when nothing is
/// found.
pub fn detect_wow_root() -> String {
    crate::config::wow_base_candidates()
        .into_iter()
        .map(|c| {
            if c.file_name().is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case("_retail_")) {
                c.parent().map(Path::to_path_buf).unwrap_or(c)
            } else {
                c
            }
        })
        .find(|c| has_game_folder(c))
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// What the wizard's "Which WoW do you play?" step offers for one game folder under the WoW
/// root: the folders `discover` would report at runtime, plus a `_classic_beta_` folder even
/// before the addon inside it has ever written a Forever passport (a fresh Forever install with
/// no characters yet has nothing else to go on), and `_classic_era_` named specifically so the
/// step can show it disabled as "not supported" rather than silently omitting it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DetectedKind {
    Retail,
    Forever,
    ClassicEra,
    Other,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DetectedGame {
    pub folder: String,
    pub kind: DetectedKind,
    /// The player-facing GoldCap addon (as opposed to GoldCap_AppData, which the companion
    /// writes itself) is installed under this folder's `Interface/AddOns`.
    pub addon_installed: bool,
}

/// Every game folder directly under `root`, classified for onboarding rather than for the sync
/// tick: unlike `discover`, a folder counts even when it has never been played (no
/// SavedVariables yet) — a fresh install is exactly the case the wizard needs to offer.
pub fn detect_games(root: &Path) -> Vec<DetectedGame> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    // The passport-based classification `discover` already does — folder name never decides,
    // only the passport — reused here for any Forever install that already has one, whatever
    // it's named.
    let passport_forever: std::collections::HashSet<String> = discover(root, None)
        .into_iter()
        .filter(|g| g.kind == GameKind::Forever)
        .map(|g| g.folder)
        .collect();

    let mut games: Vec<DetectedGame> = entries
        .flatten()
        .filter_map(|entry| {
            let dir = entry.path();
            let folder = entry.file_name().to_string_lossy().into_owned();
            if !dir.is_dir() || !is_game_folder(&folder) {
                return None;
            }
            let addon_installed = dir
                .join("Interface")
                .join("AddOns")
                .join(crate::health::ADDON_DIR_NAME)
                .is_dir();
            let kind = if folder.eq_ignore_ascii_case("_retail_") {
                DetectedKind::Retail
            } else if folder.eq_ignore_ascii_case("_classic_era_") {
                DetectedKind::ClassicEra
            } else if passport_forever.contains(&folder) || folder.eq_ignore_ascii_case("_classic_beta_") {
                DetectedKind::Forever
            } else {
                DetectedKind::Other
            };
            Some(DetectedGame { folder, kind, addon_installed })
        })
        .collect();
    games.sort_by(|a, b| a.folder.cmp(&b.folder));
    games
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
            // The max, not the first found: `read_dir` order is arbitrary, and taking whichever
            // account happened to come first could report an older interface than the install
            // actually has (fix round 1 M3).
            let forever = interfaces.iter().copied().filter(|i| is_forever_interface(*i)).max();
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

    // fix round 1 M3: read_dir order across two accounts is arbitrary, so whichever file parses
    // last must not decide the TOC's interface — the higher one, a later Forever patch, must win
    // regardless of order.
    #[test]
    fn the_forever_interface_is_the_max_seen_across_accounts() {
        let r = root("interfaces");
        saved(&r, "_classic_beta_", "A", r#"GoldCapDB = { client = { interface = 16001, build = "1.60.1.70009" } }"#);
        saved(&r, "_classic_beta_", "B", r#"GoldCapDB = { client = { interface = 16002, build = "1.61.0.70100" } }"#);
        let games = discover(&r, None);
        assert_eq!(games[0].interface, Some(16002));
        fs::remove_dir_all(&r).ok();
    }

    #[test]
    fn wow_root_prefers_the_explicit_root_path_over_everything_else() {
        let r = root("explicit-root");
        let config = Config {
            wow_root_path: r.to_string_lossy().into_owned(),
            wow_retail_path: r.join("elsewhere").join("_retail_").to_string_lossy().into_owned(),
            ..Config::default()
        };
        assert_eq!(wow_root(&config), Some(r.clone()));
        fs::remove_dir_all(&r).ok();
    }

    #[test]
    fn detect_games_offers_retail_only() {
        let r = root("detect-retail-only");
        saved(&r, "_retail_", "A", RETAIL);
        let games = detect_games(&r);
        assert_eq!(games.len(), 1);
        assert_eq!(games[0].folder, "_retail_");
        assert_eq!(games[0].kind, DetectedKind::Retail);
        fs::remove_dir_all(&r).ok();
    }

    // A fresh Forever install has no passport yet — nobody has logged in and had the addon write
    // one — but `_classic_beta_` must still be offered, or a first-time Forever player could
    // never turn it on.
    #[test]
    fn detect_games_offers_a_fresh_classic_beta_before_any_passport_exists() {
        let r = root("detect-beta-fresh");
        fs::create_dir_all(r.join("_classic_beta_")).unwrap(); // no WTF, no SavedVariables at all
        let games = detect_games(&r);
        assert_eq!(games.len(), 1);
        assert_eq!(games[0].folder, "_classic_beta_");
        assert_eq!(games[0].kind, DetectedKind::Forever);
        fs::remove_dir_all(&r).ok();
    }

    #[test]
    fn detect_games_finds_a_forever_passport_under_any_folder_name() {
        let r = root("detect-passport-anywhere");
        saved(&r, "_forever_", "A", FOREVER);
        let games = detect_games(&r);
        assert_eq!(games.len(), 1);
        assert_eq!(games[0].folder, "_forever_");
        assert_eq!(games[0].kind, DetectedKind::Forever);
        fs::remove_dir_all(&r).ok();
    }

    #[test]
    fn detect_games_names_classic_era_as_not_supported_rather_than_omitting_it() {
        let r = root("detect-classic-era");
        fs::create_dir_all(r.join("_classic_era_")).unwrap();
        let games = detect_games(&r);
        assert_eq!(games.len(), 1);
        assert_eq!(games[0].kind, DetectedKind::ClassicEra);
        fs::remove_dir_all(&r).ok();
    }

    #[test]
    fn detect_games_reports_all_three_side_by_side_and_whether_the_addon_is_installed() {
        let r = root("detect-all-three");
        saved(&r, "_retail_", "A", RETAIL);
        fs::create_dir_all(r.join("_retail_").join("Interface").join("AddOns").join("GoldCap")).unwrap();
        fs::create_dir_all(r.join("_classic_beta_")).unwrap();
        fs::create_dir_all(r.join("_classic_era_")).unwrap();
        let games = detect_games(&r);
        let summary: Vec<(&str, DetectedKind, bool)> =
            games.iter().map(|g| (g.folder.as_str(), g.kind, g.addon_installed)).collect();
        assert_eq!(
            summary,
            vec![
                ("_classic_beta_", DetectedKind::Forever, false),
                ("_classic_era_", DetectedKind::ClassicEra, false),
                ("_retail_", DetectedKind::Retail, true),
            ]
        );
        fs::remove_dir_all(&r).ok();
    }

    #[test]
    fn detect_games_on_a_missing_root_is_empty() {
        assert!(detect_games(Path::new("/definitely/not/here/goldcap")).is_empty());
    }
}
