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
}

impl Default for Config {
    fn default() -> Self {
        Config {
            region: Region::default(),
            realm_slug: String::new(),
            wow_retail_path: String::new(),
            interval_minutes: default_interval_minutes(),
            launch_at_startup: false,
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

/// Best-effort auto-detect of a standard WoW retail install at the
/// well-known default path for this platform. Empty string when nothing is
/// found there — the user fills the path in via Settings.
#[cfg(target_os = "windows")]
pub fn detect_wow_retail_path() -> String {
    detect_at(r"C:\Program Files (x86)\World of Warcraft\_retail_")
}

#[cfg(target_os = "macos")]
pub fn detect_wow_retail_path() -> String {
    detect_at("/Applications/World of Warcraft/_retail_")
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub fn detect_wow_retail_path() -> String {
    String::new()
}

#[allow(dead_code)]
fn detect_at(candidate: &str) -> String {
    let path = PathBuf::from(candidate);
    if path.is_dir() {
        path.to_string_lossy().into_owned()
    } else {
        String::new()
    }
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
            "launchAtStartup": true
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
}
