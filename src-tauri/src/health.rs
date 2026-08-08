//! Filesystem-only health checks for the Status screen. Nothing here parses
//! or writes anything: these are the four `stat` calls that let the companion
//! answer "is the addon actually installed" and "when did the game last write
//! a ledger" without waiting for a sync tick to fail.

use std::path::Path;
use std::time::UNIX_EPOCH;

/// The player-facing addon, as opposed to `GoldCap_AppData`, which this app
/// writes itself. Its absence is why prices can be arriving correctly and
/// still do nothing in game.
pub const ADDON_DIR_NAME: &str = "GoldCap";

#[derive(Debug, Default, Clone, PartialEq)]
pub struct GameHealth {
    /// `WTF/Account` exists — this is a real install that has been played,
    /// not just an unpacked folder.
    pub install_ok: bool,
    pub addon_installed: bool,
    /// mtime of the price file this app writes.
    pub app_data_written_at: Option<i64>,
    /// Newest mtime across every account's ledger. One per Battle.net
    /// account; the freshest is the one that answers "did you play today".
    pub saved_vars_written_at: Option<i64>,
}

pub fn inspect(wow_retail_path: &Path) -> GameHealth {
    if wow_retail_path.as_os_str().is_empty() {
        return GameHealth::default();
    }
    GameHealth {
        install_ok: wow_retail_path.join("WTF").join("Account").is_dir(),
        addon_installed: wow_retail_path
            .join("Interface")
            .join("AddOns")
            .join(ADDON_DIR_NAME)
            .is_dir(),
        app_data_written_at: mtime_unix(
            &crate::luafile::addon_dir(wow_retail_path).join(crate::luafile::LUA_FILE_NAME),
        ),
        saved_vars_written_at: crate::savedvars::saved_variables_paths(wow_retail_path)
            .iter()
            .filter_map(|p| mtime_unix(p))
            .max(),
    }
}

fn mtime_unix(path: &Path) -> Option<i64> {
    std::fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("goldcap-health-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn an_empty_path_is_all_false() {
        let h = inspect(std::path::Path::new(""));
        assert!(!h.install_ok);
        assert!(!h.addon_installed);
        assert_eq!(h.app_data_written_at, None);
        assert_eq!(h.saved_vars_written_at, None);
    }

    #[test]
    fn a_bare_folder_is_not_a_played_install() {
        let dir = scratch("bare");
        let h = inspect(&dir);
        assert!(!h.install_ok, "no WTF/Account means the game never ran here");
        assert!(!h.addon_installed);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_played_install_with_the_addon_reports_both() {
        let dir = scratch("played");
        fs::create_dir_all(dir.join("WTF").join("Account").join("ACC#1")).unwrap();
        fs::create_dir_all(dir.join("Interface").join("AddOns").join("GoldCap")).unwrap();

        let h = inspect(&dir);
        assert!(h.install_ok);
        assert!(h.addon_installed);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_app_data_file_reports_its_mtime() {
        let dir = scratch("appdata");
        let addon = crate::luafile::addon_dir(&dir);
        fs::create_dir_all(&addon).unwrap();
        fs::write(addon.join(crate::luafile::LUA_FILE_NAME), "-- x").unwrap();

        let at = inspect(&dir).app_data_written_at.expect("a written file has an mtime");
        let now = crate::luafile::now_unix();
        assert!((now - at).abs() <= 5, "mtime {at} should be within seconds of {now}");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn saved_variables_report_the_newest_mtime_across_accounts() {
        let dir = scratch("savedvars");
        for account in ["ACC#1", "ACC#2"] {
            let sv = dir.join("WTF").join("Account").join(account).join("SavedVariables");
            fs::create_dir_all(&sv).unwrap();
            fs::write(sv.join("GoldCap.lua"), "GoldCapDB = {}").unwrap();
        }

        let at = inspect(&dir)
            .saved_vars_written_at
            .expect("two ledger files means a newest one");
        let now = crate::luafile::now_unix();
        assert!((now - at).abs() <= 5);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_installed_addon_with_no_price_file_yet_has_no_timestamp() {
        let dir = scratch("nofile");
        fs::create_dir_all(dir.join("Interface").join("AddOns").join("GoldCap")).unwrap();

        let h = inspect(&dir);
        assert!(h.addon_installed);
        assert_eq!(h.app_data_written_at, None);
        fs::remove_dir_all(&dir).ok();
    }
}
