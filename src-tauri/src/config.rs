//! Companion configuration: region + realm to sync, the WoW `_retail_`
//! install path, sync cadence, and the launch-at-startup toggle. Persisted
//! as pretty JSON at `{app config dir}/config.json`.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub const CONFIG_FILE_NAME: &str = "config.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Region {
    #[default]
    Eu,
    Us,
}

impl fmt::Display for Region {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Region::Eu => "eu",
            Region::Us => "us",
        })
    }
}

impl std::str::FromStr for Region {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "eu" => Ok(Region::Eu),
            "us" => Ok(Region::Us),
            other => Err(format!("unknown region: {other}")),
        }
    }
}

fn default_interval_minutes() -> u32 {
    30
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    #[serde(default)]
    pub region: Region,
    #[serde(default)]
    pub realm_slug: String,
    #[serde(default)]
    pub wow_retail_path: String,
    #[serde(default = "default_interval_minutes")]
    pub interval_minutes: u32,
    #[serde(default)]
    pub launch_at_startup: bool,
    /// Long-lived upload token from pairing with goldcap.gg. Empty until the
    /// player pairs; an empty token disables upload entirely rather than
    /// failing a tick, so an unpaired companion still syncs prices normally.
    #[serde(default)]
    pub companion_token: String,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            region: Region::default(),
            realm_slug: String::new(),
            wow_retail_path: String::new(),
            interval_minutes: default_interval_minutes(),
            launch_at_startup: false,
            companion_token: String::new(),
        }
    }
}

impl Config {
    /// Sync interval clamped to a sane minimum — a stray `0` from a hand
    /// edited config file must not spin the loop into a busy sync storm.
    pub fn interval(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.interval_minutes.max(1) as u64 * 60)
    }

    pub fn load_from(path: &Path) -> io::Result<Config> {
        let text = fs::read_to_string(path)?;
        serde_json::from_str(&text).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }

    pub fn save_to(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(self).map_err(io::Error::from)?;
        fs::write(path, text)
    }

    /// Loads the config at `path`, or — on first run — creates one with an
    /// auto-detected WoW install path and persists it immediately so the
    /// file always exists once the app has run at least once.
    pub fn load_or_init(path: &Path) -> io::Result<Config> {
        match Self::load_from(path) {
            Ok(cfg) => Ok(cfg),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let cfg = Config {
                    wow_retail_path: detect_wow_retail_path(),
                    ..Config::default()
                };
                cfg.save_to(path)?;
                Ok(cfg)
            }
            Err(e) => Err(e),
        }
    }
}

/// Accepts a candidate that is either the WoW base install dir or the
/// `_retail_` dir itself and returns the `_retail_` dir when it exists on
/// disk. This is what makes registry values usable: Blizzard's keys point
/// at the base dir on some installs and at `_retail_` on others.
pub fn normalize_retail_dir(candidate: &Path) -> Option<PathBuf> {
    if !candidate.is_dir() {
        return None;
    }
    if candidate
        .file_name()
        .is_some_and(|name| name.eq_ignore_ascii_case("_retail_"))
    {
        return Some(candidate.to_path_buf());
    }
    let retail = candidate.join("_retail_");
    if retail.is_dir() {
        Some(retail)
    } else {
        None
    }
}

/// Best-effort auto-detect of a WoW retail install. Windows checks the
/// registry keys Blizzard/Battle.net write, then a set of common locations
/// across all drive letters; macOS checks the standard /Applications path.
/// Empty string when nothing is found — the user fills the path in via
/// Settings (or the Browse dialog).
#[cfg(target_os = "windows")]
pub fn detect_wow_retail_path() -> String {
    use winreg::enums::HKEY_LOCAL_MACHINE;
    use winreg::RegKey;

    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    let mut candidates: Vec<PathBuf> = Vec::new();

    // Registry first: exact answers for non-default install locations.
    // Blizzard's own InstallPath usually points at ...\_retail_ directly;
    // the uninstaller's InstallLocation at the base dir — normalize_retail_dir
    // accepts either shape.
    for (key, value) in [
        (
            r"SOFTWARE\WOW6432Node\Blizzard Entertainment\World of Warcraft",
            "InstallPath",
        ),
        (
            r"SOFTWARE\Blizzard Entertainment\World of Warcraft",
            "InstallPath",
        ),
        (
            r"SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\World of Warcraft",
            "InstallLocation",
        ),
        (
            r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\World of Warcraft",
            "InstallLocation",
        ),
    ] {
        if let Ok(subkey) = hklm.open_subkey(key) {
            if let Ok(path) = subkey.get_value::<String, _>(value) {
                candidates.push(PathBuf::from(path));
            }
        }
    }

    // Fallback: common locations across every drive letter. is_dir() on a
    // nonexistent drive fails fast without any UI prompt.
    for letter in b'C'..=b'Z' {
        for suffix in [
            r"Program Files (x86)\World of Warcraft",
            r"Program Files\World of Warcraft",
            r"World of Warcraft",
            r"Games\World of Warcraft",
            r"Blizzard\World of Warcraft",
            r"Battle.net\World of Warcraft",
        ] {
            candidates.push(PathBuf::from(format!(r"{}:\{}", letter as char, suffix)));
        }
    }

    candidates
        .iter()
        .find_map(|c| normalize_retail_dir(c))
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default()
}

#[cfg(target_os = "macos")]
pub fn detect_wow_retail_path() -> String {
    normalize_retail_dir(Path::new("/Applications/World of Warcraft"))
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default()
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub fn detect_wow_retail_path() -> String {
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_has_expected_values() {
        let cfg = Config::default();
        assert_eq!(cfg.region, Region::Eu);
        assert_eq!(cfg.realm_slug, "");
        assert_eq!(cfg.wow_retail_path, "");
        assert_eq!(cfg.interval_minutes, 30);
        assert!(!cfg.launch_at_startup);
    }

    #[test]
    fn deserializes_camel_case_json_matching_the_spec_d_wire_format() {
        // The spec (docs/superpowers/plans/2026-07-12-goldcap-companion-v1.md)
        // gives the config shape in camelCase — region/realmSlug/wowRetailPath/
        // intervalMinutes/launchAtStartup — so that's the on-disk format.
        let json = r#"{
            "region": "us",
            "realmSlug": "area-52",
            "wowRetailPath": "/Applications/World of Warcraft/_retail_",
            "intervalMinutes": 15,
            "launchAtStartup": true,
            "companionToken": "deadbeef"
        }"#;
        let cfg: Config = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.region, Region::Us);
        assert_eq!(cfg.realm_slug, "area-52");
        assert_eq!(
            cfg.wow_retail_path,
            "/Applications/World of Warcraft/_retail_"
        );
        assert_eq!(cfg.interval_minutes, 15);
        assert!(cfg.launch_at_startup);
        assert_eq!(cfg.companion_token, "deadbeef");
    }

    #[test]
    fn serializes_back_to_camel_case_keys() {
        let cfg = Config {
            realm_slug: "dentarg".into(),
            ..Config::default()
        };
        let json = serde_json::to_string(&cfg).unwrap();
        assert!(json.contains("\"realmSlug\":\"dentarg\""));
        assert!(json.contains("\"wowRetailPath\""));
        assert!(json.contains("\"intervalMinutes\":30"));
        assert!(json.contains("\"launchAtStartup\":false"));
        assert!(json.contains("\"companionToken\":\"\""));
    }

    #[test]
    fn missing_fields_fall_back_to_defaults() {
        let cfg: Config = serde_json::from_str("{}").unwrap();
        assert_eq!(cfg, Config::default());
    }

    #[test]
    fn partial_json_keeps_defaults_for_absent_fields() {
        let cfg: Config = serde_json::from_str(r#"{"realmSlug": "dentarg"}"#).unwrap();
        assert_eq!(cfg.realm_slug, "dentarg");
        assert_eq!(cfg.region, Region::Eu);
        assert_eq!(cfg.interval_minutes, 30);
    }

    #[test]
    fn round_trips_through_save_and_load() {
        let dir =
            std::env::temp_dir().join(format!("goldcap-companion-cfg-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let path = dir.join(CONFIG_FILE_NAME);

        let cfg = Config {
            region: Region::Us,
            realm_slug: "area-52".into(),
            wow_retail_path: "/tmp/wow/_retail_".into(),
            interval_minutes: 45,
            launch_at_startup: true,
            companion_token: "tok".into(),
        };
        cfg.save_to(&path).unwrap();
        let loaded = Config::load_from(&path).unwrap();
        assert_eq!(cfg, loaded);

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn load_or_init_creates_default_config_file_on_first_run() {
        let dir =
            std::env::temp_dir().join(format!("goldcap-companion-cfg-init-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let path = dir.join(CONFIG_FILE_NAME);
        assert!(!path.exists());

        let cfg = Config::load_or_init(&path).unwrap();
        assert!(path.exists());
        assert_eq!(cfg.interval_minutes, 30);

        // Second call loads the now-persisted file rather than re-detecting.
        let reloaded = Config::load_or_init(&path).unwrap();
        assert_eq!(cfg, reloaded);

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn region_display_and_from_str_round_trip() {
        assert_eq!(Region::Eu.to_string(), "eu");
        assert_eq!(Region::Us.to_string(), "us");
        assert_eq!("EU".parse::<Region>().unwrap(), Region::Eu);
        assert_eq!("us".parse::<Region>().unwrap(), Region::Us);
        assert!("uk".parse::<Region>().is_err());
    }

    #[test]
    fn interval_clamps_zero_to_one_minute() {
        let cfg = Config {
            interval_minutes: 0,
            ..Config::default()
        };
        assert_eq!(cfg.interval(), std::time::Duration::from_secs(60));
    }

    #[test]
    fn normalize_retail_dir_accepts_the_retail_dir_itself() {
        let dir =
            std::env::temp_dir().join(format!("goldcap-companion-norm-a-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let retail = dir.join("World of Warcraft").join("_retail_");
        fs::create_dir_all(&retail).unwrap();

        assert_eq!(normalize_retail_dir(&retail), Some(retail.clone()));

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn normalize_retail_dir_descends_from_the_base_install_dir() {
        let dir =
            std::env::temp_dir().join(format!("goldcap-companion-norm-b-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let base = dir.join("World of Warcraft");
        let retail = base.join("_retail_");
        fs::create_dir_all(&retail).unwrap();

        assert_eq!(normalize_retail_dir(&base), Some(retail.clone()));

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn normalize_retail_dir_rejects_missing_or_classic_only_installs() {
        let dir =
            std::env::temp_dir().join(format!("goldcap-companion-norm-c-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let base = dir.join("World of Warcraft");
        fs::create_dir_all(base.join("_classic_era_")).unwrap();

        assert_eq!(normalize_retail_dir(&base), None);
        assert_eq!(normalize_retail_dir(&dir.join("nope")), None);

        fs::remove_dir_all(&dir).ok();
    }
}
