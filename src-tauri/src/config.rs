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

// ---- Battle.net-derived candidates: the pure parts ------------------------------------------
//
// The registry's exact four keys and the fixed drive-letter list only ever find a product
// actually named "World of Warcraft" in the one folder shape GoldCap already expected. A
// Beta/PTR/Forever client is a different Battle.net product with its own uninstall entry and
// its own folder name (a player can call it anything), so `wow_base_candidates` widens the
// search: every uninstall registry entry that looks like a Warcraft product, plus whatever
// Battle.net's own install database and config file already know. Each source's decision logic
// is pure and unit-tested here; only the actual registry/file reads (below) touch the OS.

/// True when an uninstall registry entry's `DisplayName`/`Publisher` describes a World of
/// Warcraft product — retail, Beta, PTR, Classic, or Forever — rather than something unrelated
/// Blizzard or another publisher ships under the same uninstall key tree.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub fn matches_wow_uninstall_entry(display_name: Option<&str>, publisher: Option<&str>) -> bool {
    let name = display_name.unwrap_or("").to_ascii_lowercase();
    if name.contains("world of warcraft") {
        return true;
    }
    let is_blizzard = publisher
        .unwrap_or("")
        .to_ascii_lowercase()
        .contains("blizzard entertainment");
    is_blizzard && name.contains("warcraft")
}

/// Splits a Windows-style path (which may mix `\` and `/`) into (parent, last segment) without
/// going through `std::path::Path` — its separator handling is platform-native, so a `\`
/// wouldn't split on this dev machine (macOS) the way it does at runtime on Windows, and these
/// helpers are unit-tested here rather than only on a Windows CI runner. Trailing separators are
/// ignored; no separator at all yields an empty parent.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn windows_path_split(path: &str) -> (String, String) {
    let trimmed = path.trim_end_matches(['\\', '/']);
    match trimmed.rfind(['\\', '/']) {
        Some(idx) => (trimmed[..idx].to_string(), trimmed[idx + 1..].to_string()),
        None => (String::new(), trimmed.to_string()),
    }
}

/// The directory a matched uninstall entry points at: `InstallLocation` when present, else
/// `InstallSource`, else the directory portion of `UninstallString` (a command line — the
/// uninstaller executable's own directory, not the raw string). `None` when the entry carries
/// nothing usable.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub fn install_location_from_entry(
    install_location: Option<&str>,
    install_source: Option<&str>,
    uninstall_string: Option<&str>,
) -> Option<String> {
    if let Some(loc) = install_location.map(str::trim).filter(|s| !s.is_empty()) {
        return Some(loc.to_string());
    }
    if let Some(src) = install_source.map(str::trim).filter(|s| !s.is_empty()) {
        return Some(src.to_string());
    }
    let raw = uninstall_string.map(str::trim).filter(|s| !s.is_empty())?;
    let exe_path = if let Some(rest) = raw.strip_prefix('"') {
        rest.split('"').next().unwrap_or(rest)
    } else {
        raw.split_whitespace().next().unwrap_or(raw)
    };
    let (parent, _) = windows_path_split(exe_path);
    (!parent.is_empty()).then_some(parent)
}

/// A minimal protobuf wire-format reader for exactly what `product.db` needs: length-delimited
/// fields (wire type 2 — strings and embedded messages), the only type the fields below read.
/// Varint, 64-bit and 32-bit fields are skipped correctly (so the cursor stays in sync) but not
/// decoded, since nothing here needs them. Written from the wire-format spec plus the field
/// numbers documented in WowUp's `product-db.ts`/`warcraft-platform.service.ts` (file paths and
/// commit noted in the report) — no code copied from that project, which is GPL-3.0 licensed;
/// only the field layout (a fact about the file format, not an expression) is reused.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
mod protobuf {
    /// A base-128 varint starting at `data[*pos]`. Advances `*pos` past it; `None` on a
    /// truncated or unreasonably long (>10 bytes, more than a `u64` can ever need) varint.
    pub fn read_varint(data: &[u8], pos: &mut usize) -> Option<u64> {
        let mut result: u64 = 0;
        let mut shift = 0u32;
        loop {
            if *pos >= data.len() || shift >= 70 {
                return None;
            }
            let byte = data[*pos];
            *pos += 1;
            result |= ((byte & 0x7F) as u64) << shift;
            if byte & 0x80 == 0 {
                return Some(result);
            }
            shift += 7;
        }
    }

    /// Every (field_number, payload) pair for a length-delimited field at the top level of
    /// `data`. Stops at the first malformed tag/length and returns whatever was already found —
    /// the input is an arbitrary file read off disk, never something to panic over.
    pub fn length_delimited_fields(data: &[u8]) -> Vec<(u64, &[u8])> {
        let mut out = Vec::new();
        let mut pos = 0;
        while pos < data.len() {
            let Some(tag) = read_varint(data, &mut pos) else { break };
            let field_number = tag >> 3;
            match tag & 0x7 {
                0 => {
                    if read_varint(data, &mut pos).is_none() {
                        break;
                    }
                }
                1 => {
                    if pos + 8 > data.len() {
                        break;
                    }
                    pos += 8;
                }
                2 => {
                    let Some(len) = read_varint(data, &mut pos) else { break };
                    let len = len as usize;
                    if pos + len > data.len() {
                        break;
                    }
                    out.push((field_number, &data[pos..pos + len]));
                    pos += len;
                }
                5 => {
                    if pos + 4 > data.len() {
                        break;
                    }
                    pos += 4;
                }
                _ => break, // unknown wire type — bail rather than desync the rest of the buffer
            }
        }
        out
    }
}

/// One `Product` entry from `product.db`. Field numbers: `ProductDb.products` = field 1
/// (repeated, embedded `Product`); `Product.client` = field 3 (embedded `Client`),
/// `Product.family` = field 6 (string, e.g. `"wow"`); `Client.location` = field 1 (string, the
/// full path to that client's game folder), `Client.name` = field 13 (string, the folder name
/// itself, e.g. `"_retail_"`/`"_classic_beta_"`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub struct ProductDbInstall {
    pub family: String,
    pub client_name: String,
    pub client_location: String,
}

/// Decodes every `Product` in a `product.db` buffer. An install with no `Client.location` is
/// dropped — nothing usable to offer as a candidate.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub fn parse_product_db(data: &[u8]) -> Vec<ProductDbInstall> {
    let mut out = Vec::new();
    for (field, product_bytes) in protobuf::length_delimited_fields(data) {
        if field != 1 {
            continue;
        }
        let mut install = ProductDbInstall::default();
        for (pfield, payload) in protobuf::length_delimited_fields(product_bytes) {
            match pfield {
                6 => install.family = String::from_utf8_lossy(payload).into_owned(),
                3 => {
                    for (cfield, cpayload) in protobuf::length_delimited_fields(payload) {
                        match cfield {
                            1 => install.client_location = String::from_utf8_lossy(cpayload).into_owned(),
                            13 => install.client_name = String::from_utf8_lossy(cpayload).into_owned(),
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
        if !install.client_location.is_empty() {
            out.push(install);
        }
    }
    out
}

/// `product.db` installs turned into WoW-root candidates: `family == "wow"` only (Diablo,
/// Overwatch, etc. share the same database), each client's own location, plus — when that
/// location's last segment is itself a game folder (`_retail_`, `_classic_beta_`, any `_name_`,
/// which `Client.location` always is in practice) — that segment's parent too. Existence and
/// "does it actually contain a game folder" are left to the same downstream checks every other
/// `wow_base_candidates` entry already goes through (`normalize_retail_dir`,
/// `games::detect_games`) rather than duplicated here.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub fn product_db_root_candidates(installs: &[ProductDbInstall]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for install in installs {
        if !install.family.eq_ignore_ascii_case("wow") {
            continue;
        }
        let (parent, last_segment) = windows_path_split(&install.client_location);
        if crate::games::is_game_folder(&last_segment) && !parent.is_empty() {
            out.push(PathBuf::from(parent));
        }
        out.push(PathBuf::from(&install.client_location));
    }
    out
}

/// `Client.Install.DefaultInstallPath` from Battle.net's own `Battle.net.config` JSON, if
/// present and non-empty.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub fn default_install_path_from_battlenet_config(json: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    value
        .get("Client")?
        .get("Install")?
        .get("DefaultInstallPath")?
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// De-duplicates a candidate list while keeping first-seen order. `case_insensitive` is a
/// parameter rather than a `cfg` so the same logic is testable everywhere — the caller passes
/// `cfg!(target_os = "windows")`.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub fn dedupe_candidates(candidates: Vec<PathBuf>, case_insensitive: bool) -> Vec<PathBuf> {
    let mut seen: Vec<String> = Vec::new();
    let mut out = Vec::new();
    for c in candidates {
        let raw = c.to_string_lossy().into_owned();
        let key = if case_insensitive { raw.to_ascii_lowercase() } else { raw };
        if !seen.contains(&key) {
            seen.push(key);
            out.push(c);
        }
    }
    out
}

/// Where a WoW install's base folder may be: on Windows the registry keys Blizzard/Battle.net
/// write, every uninstall entry that looks like a Warcraft product, Battle.net's own install
/// database and config file, then common locations across every drive letter; on macOS the
/// standard /Applications path (and the per-user ~/Applications one). Shared by
/// `detect_wow_retail_path` and games.rs's Forever root detection — each walks this same list
/// independently, since Retail and Forever can sit on different drives entirely.
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

    // Every uninstall entry, HKLM and HKCU, 32- and 64-bit view, whose DisplayName/Publisher
    // says Warcraft — this is what finds a Beta/PTR/Forever client, which is its own Battle.net
    // product with its own uninstall entry under an arbitrary folder name.
    candidates.extend(windows_uninstall_candidates());

    // Battle.net's own install database and config file.
    candidates.extend(windows_product_db_candidates());
    if let Some(p) = windows_battlenet_config_candidate() {
        candidates.push(p);
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

    dedupe_candidates(candidates, true)
}

/// The Windows-only OS read behind the uninstall-entry candidates in `wow_base_candidates`: walks
/// both uninstall key trees (`WOW6432Node` and native) under both `HKEY_LOCAL_MACHINE` and
/// `HKEY_CURRENT_USER`, and hands each subkey's `DisplayName`/`Publisher`/`InstallLocation`/
/// `InstallSource`/`UninstallString` to the pure `matches_wow_uninstall_entry`/
/// `install_location_from_entry` above. Thin on purpose — the decision logic is what's tested.
#[cfg(target_os = "windows")]
fn windows_uninstall_candidates() -> Vec<PathBuf> {
    use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
    use winreg::RegKey;

    let mut out = Vec::new();
    for (hive, root_key) in [
        (HKEY_LOCAL_MACHINE, r"SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall"),
        (HKEY_LOCAL_MACHINE, r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall"),
        (HKEY_CURRENT_USER, r"SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall"),
        (HKEY_CURRENT_USER, r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall"),
    ] {
        let Ok(uninstall) = RegKey::predef(hive).open_subkey(root_key) else {
            continue;
        };
        for name in uninstall.enum_keys().flatten() {
            let Ok(entry) = uninstall.open_subkey(&name) else {
                continue;
            };
            let display_name = entry.get_value::<String, _>("DisplayName").ok();
            let publisher = entry.get_value::<String, _>("Publisher").ok();
            if !matches_wow_uninstall_entry(display_name.as_deref(), publisher.as_deref()) {
                continue;
            }
            let install_location = entry.get_value::<String, _>("InstallLocation").ok();
            let install_source = entry.get_value::<String, _>("InstallSource").ok();
            let uninstall_string = entry.get_value::<String, _>("UninstallString").ok();
            if let Some(dir) = install_location_from_entry(
                install_location.as_deref(),
                install_source.as_deref(),
                uninstall_string.as_deref(),
            ) {
                out.push(PathBuf::from(dir));
            }
        }
    }
    out
}

/// Reads `%ProgramData%\Battle.net\Agent\product.db` and turns it into root candidates via the
/// pure `parse_product_db`/`product_db_root_candidates` above. Missing/unreadable file is just no
/// candidates, same as every other best-effort source here.
#[cfg(target_os = "windows")]
fn windows_product_db_candidates() -> Vec<PathBuf> {
    let Ok(program_data) = std::env::var("ProgramData") else {
        return Vec::new();
    };
    let path = PathBuf::from(program_data).join("Battle.net").join("Agent").join("product.db");
    let Ok(bytes) = fs::read(&path) else {
        return Vec::new();
    };
    product_db_root_candidates(&parse_product_db(&bytes))
}

/// Reads `%APPDATA%\Battle.net\Battle.net.config` and, when it names a default install path,
/// returns `<that>\World of Warcraft` — the base folder Battle.net itself would install a new
/// product under.
#[cfg(target_os = "windows")]
fn windows_battlenet_config_candidate() -> Option<PathBuf> {
    let appdata = std::env::var("APPDATA").ok()?;
    let path = PathBuf::from(appdata).join("Battle.net").join("Battle.net.config");
    let text = fs::read_to_string(&path).ok()?;
    let install_path = default_install_path_from_battlenet_config(&text)?;
    Some(PathBuf::from(install_path).join("World of Warcraft"))
}

#[cfg(target_os = "macos")]
pub fn wow_base_candidates() -> Vec<PathBuf> {
    let mut candidates = vec![PathBuf::from("/Applications/World of Warcraft")];
    if let Ok(home) = std::env::var("HOME") {
        candidates.push(PathBuf::from(home).join("Applications").join("World of Warcraft"));
    }
    candidates
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

    // ---- Battle.net-derived candidates -------------------------------------

    #[test]
    fn matches_wow_uninstall_entry_by_display_name() {
        assert!(matches_wow_uninstall_entry(Some("World of Warcraft"), None));
        assert!(matches_wow_uninstall_entry(Some("World of Warcraft Beta"), Some("Blizzard Entertainment")));
        assert!(matches_wow_uninstall_entry(Some("World of Warcraft PTR"), None));
        assert!(matches_wow_uninstall_entry(Some("WORLD OF WARCRAFT"), None), "case-insensitive");
    }

    #[test]
    fn matches_wow_uninstall_entry_by_blizzard_publisher_plus_warcraft_name() {
        // A Forever/renamed client whose display name doesn't say "World of Warcraft" verbatim
        // but is still clearly a Warcraft product from Blizzard.
        assert!(matches_wow_uninstall_entry(
            Some("Warcraft: Forever"),
            Some("Blizzard Entertainment, Inc.")
        ));
    }

    #[test]
    fn matches_wow_uninstall_entry_rejects_unrelated_or_non_blizzard_entries() {
        assert!(!matches_wow_uninstall_entry(Some("Diablo IV"), Some("Blizzard Entertainment")));
        assert!(!matches_wow_uninstall_entry(Some("Warcraft Mod Manager"), Some("Some Other Publisher")));
        assert!(!matches_wow_uninstall_entry(None, None));
    }

    #[test]
    fn install_location_from_entry_prefers_install_location() {
        assert_eq!(
            install_location_from_entry(Some(r"D:\Games\WoW"), Some(r"E:\ignored"), None),
            Some(r"D:\Games\WoW".to_string())
        );
    }

    #[test]
    fn install_location_from_entry_falls_back_to_install_source_then_uninstall_string() {
        assert_eq!(
            install_location_from_entry(None, Some(r"E:\Source\WoW"), None),
            Some(r"E:\Source\WoW".to_string())
        );
        assert_eq!(
            install_location_from_entry(
                None,
                None,
                Some(r#""D:\Games\WoW\Uninstaller.exe" /S"#)
            ),
            Some(r"D:\Games\WoW".to_string())
        );
        assert_eq!(
            install_location_from_entry(None, None, Some(r"D:\Games\WoW\Uninstaller.exe")),
            Some(r"D:\Games\WoW".to_string())
        );
    }

    #[test]
    fn install_location_from_entry_none_when_nothing_usable() {
        assert_eq!(install_location_from_entry(None, None, None), None);
        assert_eq!(install_location_from_entry(Some("  "), None, None), None);
    }

    // ---- protobuf reader ----------------------------------------------------

    fn encode_varint(mut v: u64) -> Vec<u8> {
        let mut out = Vec::new();
        loop {
            let mut byte = (v & 0x7F) as u8;
            v >>= 7;
            if v != 0 {
                byte |= 0x80;
            }
            out.push(byte);
            if v == 0 {
                break;
            }
        }
        out
    }

    /// Encodes one length-delimited (wire type 2) field: a string or an already-encoded embedded
    /// message. Field numbers used in these tests (1, 3, 6, 13) all fit a one-byte tag.
    fn encode_field(field_number: u64, payload: &[u8]) -> Vec<u8> {
        let mut out = encode_varint((field_number << 3) | 2);
        out.extend(encode_varint(payload.len() as u64));
        out.extend_from_slice(payload);
        out
    }

    fn encode_client(location: &str, name: &str) -> Vec<u8> {
        let mut out = encode_field(1, location.as_bytes());
        out.extend(encode_field(13, name.as_bytes()));
        out
    }

    fn encode_product(family: &str, client: &[u8]) -> Vec<u8> {
        let mut out = encode_field(3, client);
        out.extend(encode_field(6, family.as_bytes()));
        out
    }

    #[test]
    fn read_varint_decodes_single_and_multi_byte_values() {
        let mut pos = 0;
        assert_eq!(protobuf::read_varint(&[0x01], &mut pos), Some(1));
        assert_eq!(pos, 1);

        let mut pos = 0;
        // 300 = 0b1_0010_1100 -> low 7 bits 0101100 with continuation, then 0000010
        assert_eq!(protobuf::read_varint(&[0xAC, 0x02], &mut pos), Some(300));
        assert_eq!(pos, 2);
    }

    #[test]
    fn read_varint_none_on_truncated_input() {
        let mut pos = 0;
        assert_eq!(protobuf::read_varint(&[0x80], &mut pos), None);
        assert_eq!(protobuf::read_varint(&[], &mut pos), None);
    }

    #[test]
    fn length_delimited_fields_reads_tag_and_payload_and_skips_varints() {
        let mut data = encode_field(1, b"hello");
        // A varint (wire type 0) field 2 = 42, which must be skipped without desyncing.
        data.extend(encode_varint(2 << 3)); // field 2, wire type 0 (varint)
        data.extend(encode_varint(42));
        data.extend(encode_field(3, b"world"));

        let fields = protobuf::length_delimited_fields(&data);
        assert_eq!(fields, vec![(1, b"hello".as_slice()), (3, b"world".as_slice())]);
    }

    #[test]
    fn length_delimited_fields_stops_cleanly_on_truncated_length() {
        // A field-2 tag claiming a payload longer than what remains.
        let mut data = encode_varint((5 << 3) | 2);
        data.extend(encode_varint(50));
        data.extend_from_slice(b"short");
        assert_eq!(protobuf::length_delimited_fields(&data), Vec::<(u64, &[u8])>::new());
    }

    #[test]
    fn parse_product_db_decodes_family_and_client_from_a_hand_built_buffer() {
        let client = encode_client(r"D:\Games\World of Warcraft\_classic_beta_", "_classic_beta_");
        let product = encode_product("wow", &client);
        let mut db = encode_field(1, &product);

        // A second, non-wow product (e.g. Diablo IV) must be decoded but filtered out downstream
        // by product_db_root_candidates, not dropped here.
        let other_client = encode_client(r"D:\Games\Diablo IV", "");
        let other_product = encode_product("d4", &other_client);
        db.extend(encode_field(1, &other_product));

        let installs = parse_product_db(&db);
        assert_eq!(
            installs,
            vec![
                ProductDbInstall {
                    family: "wow".into(),
                    client_name: "_classic_beta_".into(),
                    client_location: r"D:\Games\World of Warcraft\_classic_beta_".into(),
                },
                ProductDbInstall {
                    family: "d4".into(),
                    client_name: "".into(),
                    client_location: r"D:\Games\Diablo IV".into(),
                },
            ]
        );
    }

    #[test]
    fn parse_product_db_empty_on_garbage_bytes() {
        let garbage: Vec<u8> = vec![0xFF, 0xFF, 0xFF, 0xFF, 0xFF];
        assert_eq!(parse_product_db(&garbage), Vec::<ProductDbInstall>::new());
    }

    #[test]
    fn product_db_root_candidates_keeps_only_wow_family_and_adds_the_parent_of_a_game_folder() {
        let installs = vec![
            ProductDbInstall {
                family: "wow".into(),
                client_name: "_classic_beta_".into(),
                client_location: r"D:\Games\World of Warcraft\_classic_beta_".into(),
            },
            ProductDbInstall {
                family: "WoW".into(), // case-insensitive family match
                client_name: "_retail_".into(),
                client_location: r"C:\Program Files (x86)\World of Warcraft\_retail_".into(),
            },
            ProductDbInstall {
                family: "d4".into(),
                client_name: "".into(),
                client_location: r"D:\Games\Diablo IV".into(),
            },
        ];
        let out = product_db_root_candidates(&installs);
        assert_eq!(
            out,
            vec![
                PathBuf::from(r"D:\Games\World of Warcraft"),
                PathBuf::from(r"D:\Games\World of Warcraft\_classic_beta_"),
                PathBuf::from(r"C:\Program Files (x86)\World of Warcraft"),
                PathBuf::from(r"C:\Program Files (x86)\World of Warcraft\_retail_"),
            ]
        );
    }

    #[test]
    fn default_install_path_from_battlenet_config_reads_the_nested_key() {
        let json = r#"{"Client": {"Install": {"DefaultInstallPath": "D:\\Games", "Other": 1}}}"#;
        assert_eq!(
            default_install_path_from_battlenet_config(json),
            Some(r"D:\Games".to_string())
        );
    }

    #[test]
    fn default_install_path_from_battlenet_config_none_when_absent_or_malformed() {
        assert_eq!(default_install_path_from_battlenet_config("{}"), None);
        assert_eq!(default_install_path_from_battlenet_config("not json"), None);
        assert_eq!(
            default_install_path_from_battlenet_config(r#"{"Client": {"Install": {}}}"#),
            None
        );
        assert_eq!(
            default_install_path_from_battlenet_config(
                r#"{"Client": {"Install": {"DefaultInstallPath": "  "}}}"#
            ),
            None
        );
    }

    #[test]
    fn dedupe_candidates_is_case_insensitive_on_request_and_keeps_first_seen_order() {
        let candidates = vec![
            PathBuf::from(r"D:\Games\WoW"),
            PathBuf::from(r"C:\Other"),
            PathBuf::from(r"d:\games\wow"),
        ];
        let deduped = dedupe_candidates(candidates.clone(), true);
        assert_eq!(deduped, vec![PathBuf::from(r"D:\Games\WoW"), PathBuf::from(r"C:\Other")]);

        let kept = dedupe_candidates(candidates, false);
        assert_eq!(kept.len(), 3, "case-sensitive mode treats differing case as distinct");
    }
}
