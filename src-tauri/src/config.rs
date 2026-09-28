//! Companion configuration: region + realm to sync, the WoW `_retail_`
//! install path, the WoW: Forever root, sync cadence, and the launch-at-startup
//! and auto-update toggles. Persisted as pretty JSON at
//! `{app config dir}/config.json`.

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
    Kr,
    Tw,
}

impl fmt::Display for Region {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Region::Eu => "eu",
            Region::Us => "us",
            Region::Kr => "kr",
            Region::Tw => "tw",
        })
    }
}

impl std::str::FromStr for Region {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "eu" => Ok(Region::Eu),
            "us" => Ok(Region::Us),
            "kr" => Ok(Region::Kr),
            "tw" => Ok(Region::Tw),
            other => Err(format!("unknown region: {other}")),
        }
    }
}

fn default_interval_minutes() -> u32 {
    30
}

fn default_launch_at_startup() -> bool {
    true
}

fn default_auto_update() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    #[serde(default)]
    pub region: Region,
    #[serde(default)]
    pub realm_slug: String,
    /// Follow whatever realm the player logged into most recently, instead of
    /// pinning one. Defaults to OFF for a config written before this existed
    /// (`serde(default)` — an install with a working realm keeps it) and ON
    /// for a fresh one (see `Config::default`), which is the answer for
    /// someone who plays a couple of hours on one realm and a couple on
    /// another.
    #[serde(default)]
    pub realm_auto: bool,
    /// The `_retail_` folder itself — not a root. Every retail reader
    /// (`health.rs`, `savedvars.rs`, `upload.rs`, `luafile::addon_dir`,
    /// `wtf.rs`) reads this directly. Set by the wizard's or Settings'
    /// "Change…" / auto-detect for Retail — `config::detect_wow_retail_path`
    /// / `normalize_retail_dir` — independently of anything to do with
    /// Forever, since the two games can live on entirely different drives.
    #[serde(default)]
    pub wow_retail_path: String,
    /// The WoW folder that holds the Forever client — `_classic_beta_` today,
    /// or any folder the addon has stamped a Forever passport into (see
    /// `games.rs`). Not the game folder itself: the folder's exact name is
    /// Blizzard's to change, so this is one level up, the same way
    /// `games::discover`/`sync_forever` need it. Independent of
    /// `wow_retail_path` — a player can have Retail on one drive and Forever
    /// on another. A config written before this field existed gets one
    /// migrated in from the old shared `wowRootPath` (or, failing that,
    /// derived from `wow_retail_path`'s parent) on load — see `migrate`.
    #[serde(default)]
    pub forever_root_path: String,
    /// Whether the Retail leg of the tick (import string, region data, ledger upload, runs,
    /// retail SavedVariables) runs at all. A config written before this field existed is
    /// migrated to `true` when it already named a realm or a retail path — see `migrate`.
    #[serde(default)]
    pub retail_enabled: bool,
    /// Whether the WoW: Forever leg of the tick (`forever::sync_forever`) runs at all. A config
    /// written before this field existed is migrated to `true` — Forever's own sync is already a
    /// no-op on a machine with no Forever install, so this never invents work on a retail-only
    /// machine (see `migrate`).
    #[serde(default)]
    pub forever_enabled: bool,
    #[serde(default = "default_interval_minutes")]
    pub interval_minutes: u32,
    /// Defaults to on: most players want the companion syncing in the
    /// background without remembering to launch it, and it's a single
    /// checkbox away in Settings for anyone who doesn't. Only affects
    /// brand-new configs — `#[serde(default)]` only fills this in when the
    /// key is absent from the file, so an existing install's persisted
    /// choice (explicit or not) is never silently flipped by an update.
    #[serde(default = "default_launch_at_startup")]
    pub launch_at_startup: bool,
    /// Look for a new version in the background and download it when one is
    /// out. Off means the companion never asks at all until the player
    /// presses "Check for updates" in Settings. Installing is a separate
    /// click on Windows either way. Defaults to on, and — like
    /// `launch_at_startup` — a config written before this existed reads as
    /// on, so an existing install keeps updating the way it always has.
    #[serde(default = "default_auto_update")]
    pub auto_update: bool,
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
            realm_auto: true,
            wow_retail_path: String::new(),
            forever_root_path: String::new(),
            // A brand-new config is never written to disk with these already decided — the
            // wizard is what turns them on, per game the player actually picks. See `migrate`
            // for the different rule an EXISTING config on disk gets when read for the first
            // time by a build that has these fields.
            retail_enabled: false,
            forever_enabled: false,
            interval_minutes: default_interval_minutes(),
            launch_at_startup: default_launch_at_startup(),
            auto_update: default_auto_update(),
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

    /// Whether this config can actually sync. Also decides which screen the
    /// window opens on — an incomplete config means the first-run wizard —
    /// so no separate "onboarded" flag is persisted. The single rule the UI
    /// defers to (via the `setup_complete` command) instead of guessing at
    /// its own copy of it: at least one game is turned on, Retail (when on)
    /// needs its own path plus a realm (pinned or auto-follow), and Forever
    /// (when on) needs its own root — each game's folder is its own
    /// requirement, since the two may not even share a drive.
    pub fn is_complete(&self) -> bool {
        (self.retail_enabled || self.forever_enabled)
            && (!self.retail_enabled
                || (!self.wow_retail_path.trim().is_empty()
                    && (self.realm_auto || !self.realm_slug.trim().is_empty())))
            && (!self.forever_enabled || !self.forever_root_path.trim().is_empty())
    }

    pub fn load_from(path: &Path) -> io::Result<Config> {
        let text = fs::read_to_string(path)?;
        let raw: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        let cfg: Config = serde_json::from_value(raw.clone())
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        Ok(migrate(&raw, cfg))
    }

    pub fn save_to(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(self).map_err(io::Error::from)?;
        fs::write(path, text)
    }

    /// Loads the config at `path`, or — on first run — creates one with
    /// auto-detected WoW install paths (Retail and Forever, searched
    /// independently — see `detect_wow_retail_path` and
    /// `games::detect_forever_root`) and persists it immediately so the
    /// file always exists once the app has run at least once.
    pub fn load_or_init(path: &Path) -> io::Result<Config> {
        match Self::load_from(path) {
            Ok(cfg) => Ok(cfg),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let cfg = Config {
                    wow_retail_path: detect_wow_retail_path(),
                    forever_root_path: crate::games::detect_forever_root(),
                    ..Config::default()
                };
                cfg.save_to(path)?;
                Ok(cfg)
            }
            Err(e) => Err(e),
        }
    }
}

/// `forever_root_path` derivable from a `_retail_` path: its parent, or empty when the path
/// doesn't look like one. Used by `migrate` as the last-resort fallback for a config old enough
/// to predate even the shared `wowRootPath` this branch first introduced — see `migrate`.
fn derive_wow_root_path(wow_retail_path: &str) -> String {
    let retail = Path::new(wow_retail_path.trim());
    if retail.file_name().is_some_and(|n| n.eq_ignore_ascii_case("_retail_")) {
        retail
            .parent()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default()
    } else {
        String::new()
    }
}

/// Fills in `forever_root_path`/`retail_enabled`/`forever_enabled` for a config saved before
/// those fields existed, so an existing install keeps syncing exactly as it did before this
/// version — without this, an upgrade would read every one of them as `false`/empty (their
/// `serde(default)`) and the sync loop would go silent for everyone already running the
/// companion.
///
/// Pure and given the raw JSON alongside the already-deserialized struct on purpose: `serde`'s
/// `#[serde(default)]` throws away *whether* a key was present, leaving only its filled-in value
/// — and "the key was never written" (an old config; migrate it) and "the key was written as
/// `false`"/`""` (a player who saved it that way through this very feature; leave it alone) both
/// land on the same zero value. Only the raw object still knows which one happened.
///
/// `forever_root_path` specifically: a config from a build of THIS branch already has a
/// `wowRootPath` key (the shared root that branch introduced, before Retail and Forever got
/// their own folders) — that value is exactly what `forever_root_path` means now, so it is
/// carried over verbatim rather than re-derived. A config older than that has no `wowRootPath`
/// either, so falls back to `wow_retail_path`'s parent, same as before.
fn migrate(raw: &serde_json::Value, mut cfg: Config) -> Config {
    let had_key = |k: &str| raw.get(k).is_some();

    if !had_key("foreverRootPath") {
        let legacy_root = raw
            .get("wowRootPath")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        cfg.forever_root_path = if !legacy_root.is_empty() {
            legacy_root.to_string()
        } else {
            derive_wow_root_path(&cfg.wow_retail_path)
        };
    }

    if !had_key("retailEnabled") {
        cfg.retail_enabled =
            !cfg.realm_slug.trim().is_empty() || !cfg.wow_retail_path.trim().is_empty();
    }
    if !had_key("foreverEnabled") {
        // Forever's own sync is already a no-op without an install on disk (forever.rs), so
        // turning it on for every pre-existing config never invents work for a retail-only
        // machine — it just lets an install that is already there start being offered.
        cfg.forever_enabled = true;
    }

    cfg
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

/// Where a WoW install's base folder may be: on Windows the registry keys Blizzard/Battle.net
/// write, then common locations across every drive letter; on macOS the standard
/// /Applications path. Shared by `detect_wow_retail_path` and games.rs's Forever root detection
/// — each walks this same list independently, since Retail and Forever can sit on different
/// drives entirely.
#[cfg(target_os = "windows")]
pub fn wow_base_candidates() -> Vec<PathBuf> {
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
}

#[cfg(target_os = "macos")]
pub fn wow_base_candidates() -> Vec<PathBuf> {
    vec![PathBuf::from("/Applications/World of Warcraft")]
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub fn wow_base_candidates() -> Vec<PathBuf> {
    Vec::new()
}

/// Best-effort auto-detect of a WoW retail install (see `wow_base_candidates`). Empty string
/// when nothing is found — the user fills the path in via Settings (or the Browse dialog).
pub fn detect_wow_retail_path() -> String {
    wow_base_candidates()
        .iter()
        .find_map(|c| normalize_retail_dir(c))
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    // The release procedure bumps the version in TWO manifests (see
    // .github/workflows/companion-release.yml's header) and the updater
    // compares what it downloads against the one baked into the binary — so
    // the two drifting apart ships an app that offers itself an update
    // forever, or none at all.
    //
    // The literal that used to sit here ("1.6.0") pinned the assertion to one
    // release, which meant every bump failed CI on a test that had nothing to
    // say about the change. What matters is that the manifests AGREE and that
    // the version is a real semver triple; both survive a bump.
    #[test]
    fn cargo_and_tauri_versions_agree() {
        let tauri: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        let cargo = env!("CARGO_PKG_VERSION");

        assert_eq!(tauri["version"].as_str(), Some(cargo));
        let parts: Vec<&str> = cargo.split('.').collect();
        assert_eq!(parts.len(), 3, "version must be major.minor.patch: {cargo}");
        assert!(
            parts.iter().all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit())),
            "version must be numeric: {cargo}",
        );
    }

    #[test]
    fn default_config_has_expected_values() {
        let cfg = Config::default();
        assert_eq!(cfg.region, Region::Eu);
        assert_eq!(cfg.realm_slug, "");
        // A fresh install follows the realm you last played on; an existing
        // config without the field keeps its pinned realm (serde default).
        assert!(cfg.realm_auto);
        assert_eq!(cfg.wow_retail_path, "");
        assert_eq!(cfg.forever_root_path, "");
        assert!(!cfg.retail_enabled, "a brand-new config waits for the wizard");
        assert!(!cfg.forever_enabled, "a brand-new config waits for the wizard");
        assert_eq!(cfg.interval_minutes, 30);
        assert!(cfg.launch_at_startup);
        assert!(cfg.auto_update);
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
        assert!(json.contains("\"foreverRootPath\":\"\""));
        assert!(json.contains("\"retailEnabled\":false"));
        assert!(json.contains("\"foreverEnabled\":false"));
        assert!(json.contains("\"intervalMinutes\":30"));
        assert!(json.contains("\"launchAtStartup\":true"));
        assert!(json.contains("\"autoUpdate\":true"));
        assert!(json.contains("\"companionToken\":\"\""));
    }

    #[test]
    fn missing_fields_fall_back_to_defaults() {
        let cfg: Config = serde_json::from_str("{}").unwrap();
        assert_eq!(Config { realm_auto: cfg.realm_auto, ..Config::default() }, cfg);
    }

    // The one field where "what a fresh install starts with" and "what an old
    // config file means" deliberately disagree. A config written before
    // realm-following existed names a realm on purpose, and reading it must
    // not silently start overriding that choice from the game's folders;
    // a config that names nothing has no choice to protect.
    #[test]
    fn an_existing_config_keeps_its_pinned_realm_while_a_fresh_one_follows() {
        let existing: Config = serde_json::from_str(r#"{"realmSlug": "dentarg"}"#).unwrap();
        assert!(!existing.realm_auto, "an upgrade must not repoint someone's realm");

        assert!(Config::default().realm_auto, "a fresh install follows the last realm played");
    }

    // Every companion out there was written before this switch existed, and
    // each of them has been updating itself; reading that file must not
    // quietly turn updates off.
    #[test]
    fn a_config_from_before_the_switch_keeps_updating_itself() {
        let existing: Config = serde_json::from_str(r#"{"realmSlug": "dentarg"}"#).unwrap();
        assert!(existing.auto_update);
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
            realm_auto: false,
            wow_retail_path: "/tmp/wow/_retail_".into(),
            // Set explicitly, matching what a real save always writes — a round trip must not
            // let `migrate` (which only fills in a key that was truly ABSENT) rederive over
            // top of an already-saved choice.
            forever_root_path: "/tmp/forever-drive".into(),
            retail_enabled: true,
            forever_enabled: false,
            interval_minutes: 45,
            launch_at_startup: true,
            auto_update: false,
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

    #[test]
    fn a_default_config_is_not_complete() {
        assert!(!Config::default().is_complete());
    }

    #[test]
    fn no_path_or_no_game_enabled_is_not_complete() {
        let no_path = Config {
            retail_enabled: true,
            realm_slug: "dentarg".into(),
            ..Config::default()
        };
        assert!(!no_path.is_complete(), "no wow_retail_path");

        let no_game = Config {
            wow_retail_path: "/tmp/wow/_retail_".into(),
            realm_slug: "dentarg".into(),
            ..Config::default()
        };
        assert!(!no_game.is_complete(), "neither game turned on");
    }

    #[test]
    fn whitespace_does_not_count_as_a_path() {
        let cfg = Config {
            wow_retail_path: "  ".into(),
            retail_enabled: true,
            realm_slug: "dentarg".into(),
            ..Config::default()
        };
        assert!(!cfg.is_complete());
    }

    #[test]
    fn retail_on_needs_a_realm_unless_auto_follow_is_on() {
        let no_realm = Config {
            wow_retail_path: "/tmp/wow/_retail_".into(),
            retail_enabled: true,
            realm_auto: false,
            realm_slug: "  ".into(),
            ..Config::default()
        };
        assert!(!no_realm.is_complete());

        let pinned = Config {
            wow_retail_path: "/tmp/wow/_retail_".into(),
            retail_enabled: true,
            realm_auto: false,
            realm_slug: "dentarg".into(),
            ..Config::default()
        };
        assert!(pinned.is_complete());

        let auto = Config {
            wow_retail_path: "/tmp/wow/_retail_".into(),
            retail_enabled: true,
            realm_auto: true,
            realm_slug: "".into(),
            ..Config::default()
        };
        assert!(auto.is_complete(), "auto-follow needs no realm named yet");
    }

    // A Forever-only player never sees a realm picker at all (the wizard's own rule) — the
    // completeness check must not invent a realm requirement for them, and Retail's own path
    // must not be required either.
    #[test]
    fn forever_only_needs_no_realm_and_no_retail_path() {
        let cfg = Config {
            forever_root_path: "/tmp/forever-drive".into(),
            forever_enabled: true,
            retail_enabled: false,
            realm_auto: false,
            realm_slug: "".into(),
            ..Config::default()
        };
        assert!(cfg.is_complete());
    }

    #[test]
    fn forever_on_needs_its_own_root() {
        let cfg = Config {
            forever_root_path: "  ".into(),
            forever_enabled: true,
            ..Config::default()
        };
        assert!(!cfg.is_complete());
    }

    #[test]
    fn both_games_with_retail_configured_is_complete() {
        let cfg = Config {
            wow_retail_path: "/tmp/wow/_retail_".into(),
            forever_root_path: "/tmp/forever-drive".into(),
            retail_enabled: true,
            forever_enabled: true,
            realm_auto: false,
            realm_slug: "dentarg".into(),
            ..Config::default()
        };
        assert!(cfg.is_complete());
    }

    // The two-drives case the whole feature is for: Retail's path and Forever's root need not
    // share any prefix at all.
    #[test]
    fn both_games_on_entirely_different_drives_is_complete() {
        let cfg = Config {
            wow_retail_path: r"C:\Program Files (x86)\World of Warcraft\_retail_".into(),
            forever_root_path: r"D:\Games\World of Warcraft".into(),
            retail_enabled: true,
            forever_enabled: true,
            realm_auto: true,
            ..Config::default()
        };
        assert!(cfg.is_complete());
    }

    // ---- migrate() ---------------------------------------------------------

    fn migrated(json: &str) -> Config {
        let raw: serde_json::Value = serde_json::from_str(json).unwrap();
        let cfg: Config = serde_json::from_value(raw.clone()).unwrap();
        migrate(&raw, cfg)
    }

    // A config saved before this feature existed at all: no wowRootPath, no forever/retail
    // toggles. It must come back retail-on (it already named a realm and a path) with a
    // forever_root_path derived from the retail path's parent — the best guess available for a
    // machine that has never distinguished the two.
    #[test]
    fn an_old_retail_config_is_migrated_to_retail_on_forever_on_with_a_derived_root() {
        let cfg = migrated(
            r#"{"realmSlug": "dentarg", "wowRetailPath": "/Applications/World of Warcraft/_retail_"}"#,
        );
        assert!(cfg.retail_enabled, "it already named a realm and a retail path");
        assert!(cfg.forever_enabled, "forever's own sync no-ops without an install anyway");
        assert_eq!(cfg.forever_root_path, "/Applications/World of Warcraft");
        assert!(cfg.is_complete(), "an existing retail player must not be sent back to the wizard");
    }

    // A config from a build of THIS branch (has the shared wowRootPath field, not yet the
    // per-game forever_root_path) must carry that value over verbatim as forever_root_path,
    // not re-derive it from wow_retail_path — the player may already have pointed wowRootPath
    // somewhere that isn't wow_retail_path's parent.
    #[test]
    fn a_config_from_this_branch_carries_its_shared_root_into_forever_root_path() {
        let cfg = migrated(
            r#"{"realmSlug": "dentarg", "wowRetailPath": "/Applications/World of Warcraft/_retail_", "wowRootPath": "/custom/shared/root", "retailEnabled": true, "foreverEnabled": true}"#,
        );
        assert_eq!(cfg.forever_root_path, "/custom/shared/root");
    }

    // A config with neither a realm nor a retail path (freakishly old, or hand-edited to `{}`)
    // has nothing to turn retail on FOR — it must not claim retail is enabled with no path to
    // sync against, which would show a Retail card with every stage stuck on "Waiting for setup".
    #[test]
    fn an_empty_old_config_does_not_turn_retail_on() {
        let cfg = migrated("{}");
        assert!(!cfg.retail_enabled);
        assert!(cfg.forever_enabled);
        assert_eq!(cfg.forever_root_path, "");
    }

    // A player who explicitly saved retailEnabled/foreverEnabled through Settings must have that
    // choice respected on the next load, even though the stored value is `false` — the same
    // false a pre-migration file's *absence* of the key would also deserialize to. Only the raw
    // JSON's key presence tells the two apart.
    #[test]
    fn an_explicit_false_survives_migration_even_though_it_looks_like_an_old_files_default() {
        let cfg = migrated(
            r#"{"realmSlug": "dentarg", "wowRetailPath": "/tmp/wow/_retail_", "foreverRootPath": "/tmp/forever", "retailEnabled": false, "foreverEnabled": false}"#,
        );
        assert!(!cfg.retail_enabled, "the player's own choice, not the old-config default");
        assert!(!cfg.forever_enabled);
    }

    #[test]
    fn a_saved_forever_root_path_is_never_overwritten_by_a_derived_one() {
        let cfg = migrated(
            r#"{"wowRetailPath": "/Applications/World of Warcraft/_retail_", "foreverRootPath": "/custom/root"}"#,
        );
        assert_eq!(cfg.forever_root_path, "/custom/root");
    }

    // A retail path that is not shaped like `.../_retail_` (defensive — normalize_retail_dir
    // should prevent this from ever being saved) derives nothing rather than guessing.
    #[test]
    fn a_retail_path_not_shaped_like_retail_derives_no_root() {
        let cfg = migrated(r#"{"wowRetailPath": "/tmp/not-retail"}"#);
        assert_eq!(cfg.forever_root_path, "");
    }
}
