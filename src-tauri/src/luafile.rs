//! Writes the `GoldCap_AppData` addon folder the sync loop hands to the
//! game: a static `.toc` plus a regenerated `AppData.lua` carrying the
//! latest GCS1 import string and, when the companion holds a fresh one, the
//! whole-market region payload (GCM1, `region.rs`) as `regionString`. See
//! `docs/superpowers/plans/2026-07-12-goldcap-companion-v1.md`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const ADDON_DIR_NAME: &str = "GoldCap_AppData";
pub const TOC_FILE_NAME: &str = "GoldCap_AppData.toc";
pub const LUA_FILE_NAME: &str = "AppData.lua";

/// Second data file in the same mini-addon: the ledger summary the in-game
/// Sold tab renders. Separate from `AppData.lua` on purpose — the price path
/// is never touched by summary writes, and a failed summary fetch leaves the
/// previous snapshot on disk (nothing rewrites this file on failure).
pub const LEDGER_FILE_NAME: &str = "LedgerSummary.lua";

/// Third data file in the same mini-addon: the "Buy runs" list. Same
/// separation as `LEDGER_FILE_NAME` — its own file, its own write path.
pub const RUNS_FILE_NAME: &str = "Runs.lua";

/// Static contents of `GoldCap_AppData.toc`. Never changes at runtime; the
/// companion only (re)writes it if it's missing or a game patch changed the
/// expected Interface version out from under an older build of this file.
pub const TOC_CONTENTS: &str = "\
## Interface: 120007
## Title: GoldCap AppData
## Notes: Auto-synced market data for GoldCap. File is rewritten by the GoldCap companion app.
## LoadOnDemand: 0
AppData.lua
LedgerSummary.lua
Runs.lua
";

/// `{wowRetailPath}/Interface/AddOns/GoldCap_AppData`.
pub fn addon_dir(wow_retail_path: &Path) -> PathBuf {
    wow_retail_path
        .join("Interface")
        .join("AddOns")
        .join(ADDON_DIR_NAME)
}

/// A successful sync is any HTTP 200 body that starts with the GCS1 magic
/// prefix the addon's `ImportString.Parse` expects. Anything else — an
/// empty body, an HTML error page, or the API's own `realm_not_found` /
/// `no_data` plain-text error bodies — must never be written to disk.
pub fn is_valid_gcs1_body(body: &str) -> bool {
    body.starts_with("GCS1;")
}

/// Escapes a string for safe embedding inside a Lua single-quoted string
/// literal. The GCS1 alphabet is `[%w;:=,.-]`, so in practice nothing here
/// ever needs escaping — this exists as defense in depth against a
/// malformed or unexpected upstream payload ever breaking the Lua file.
pub fn escape_lua_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\0' => {} // never valid inside a Lua short string; drop defensively
            _ => out.push(ch),
        }
    }
    out
}

/// Renders `AppData.lua`'s contents:
/// `GoldCap_AppData = { importString = '...', writtenAt = 1752345678 }`, plus
/// `regionString = '...'` between the two when the companion holds a region payload
/// (`region.rs`). Without one the file is byte-identical to a build that never had it.
pub fn render_app_data_lua(
    import_string: &str,
    region_string: Option<&str>,
    written_at: i64,
) -> String {
    match region_string {
        Some(region) => format!(
            "GoldCap_AppData = {{ importString = '{}', regionString = '{}', writtenAt = {} }}\n",
            escape_lua_string(import_string),
            escape_lua_string(region),
            written_at
        ),
        None => format!(
            "GoldCap_AppData = {{ importString = '{}', writtenAt = {} }}\n",
            escape_lua_string(import_string),
            written_at
        ),
    }
}

pub fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Writes `contents` to `path` atomically: a sibling `path.tmp` file is
/// written first, then renamed over `path`. Rename replaces the destination
/// in one filesystem operation on both NTFS and APFS/most POSIX
/// filesystems, so the game (or anything else) never observes a
/// partially-written file, and a crash mid-write leaves the previous good
/// file untouched.
pub fn write_atomic(path: &Path, contents: &str) -> io::Result<()> {
    let tmp_path = tmp_path_for(path);
    fs::write(&tmp_path, contents)?;
    fs::rename(&tmp_path, path)?;
    Ok(())
}

fn tmp_path_for(path: &Path) -> PathBuf {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    PathBuf::from(tmp)
}

/// Ensures the static `.toc` exists with today's contents inside `dir`
/// (created if missing). Writes only when missing or different, so the
/// file's mtime doesn't churn on every sync tick. Returns whether it wrote.
pub fn ensure_toc(dir: &Path) -> io::Result<bool> {
    fs::create_dir_all(dir)?;
    let path = dir.join(TOC_FILE_NAME);
    let needs_write = match fs::read_to_string(&path) {
        Ok(existing) => existing != TOC_CONTENTS,
        Err(e) if e.kind() == io::ErrorKind::NotFound => true,
        Err(e) => return Err(e),
    };
    if needs_write {
        fs::write(&path, TOC_CONTENTS)?;
    }
    Ok(needs_write)
}

/// Atomically (re)writes `AppData.lua` inside `dir`. Call `ensure_toc`
/// first (or otherwise guarantee `dir` exists) before calling this.
pub fn write_app_data_lua(
    dir: &Path,
    import_string: &str,
    region_string: Option<&str>,
    written_at: i64,
) -> io::Result<()> {
    let contents = render_app_data_lua(import_string, region_string, written_at);
    write_atomic(&dir.join(LUA_FILE_NAME), &contents)
}

/// Removes `LedgerSummary.lua` from `dir` if present. Called on unpair: the
/// previously written ledger snapshot must not survive an unpair, or "no
/// server data next login" only holds for a companion that was never paired.
/// A missing file is not an error -- there is nothing to remove.
pub fn remove_ledger_summary(dir: &Path) -> io::Result<()> {
    match fs::remove_file(dir.join(LEDGER_FILE_NAME)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Removes `Runs.lua` from `dir` if present. Same unpair rule as
/// `remove_ledger_summary`: a previously written buy-runs snapshot must not
/// survive an unpair. A missing file is not an error.
pub fn remove_runs(dir: &Path) -> io::Result<()> {
    match fs::remove_file(dir.join(RUNS_FILE_NAME)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// A WoW: Forever install's own toc for the mini-addon: the Forever client loads
/// `<Addon>_Camelot.toc` (spec, "The client"). Never written into `_retail_`.
// used from Task C5 on
#[allow(dead_code)]
pub const FOREVER_TOC_FILE_NAME: &str = "GoldCap_AppData_Camelot.toc";

/// The toc, with the interface the addon's passport reported, so it follows Forever patches.
// used from Task C5 on
#[allow(dead_code)]
pub fn forever_toc_contents(interface: i64) -> String {
    format!(
        "## Interface: {interface}\n## Title: GoldCap AppData\n## Notes: Auto-synced market data for GoldCap. File is rewritten by the GoldCap companion app.\n## LoadOnDemand: 0\nAppData.lua\n"
    )
}

/// `GoldCap_AppData = { foreverString = '…', foreverUpload = true, writtenAt = … }`:
/// `foreverString` — every player's prices for this install's market (GCF1), when kept;
/// `foreverUpload` — this companion is paired and sends this install's scans (the addon says
/// "shared on your next /reload" only then). No retail key is ever written here.
// used from Task C5 on
#[allow(dead_code)]
pub fn render_forever_app_data_lua(forever_string: Option<&str>, uploads: bool, written_at: i64) -> String {
    let mut fields = Vec::new();
    if let Some(s) = forever_string {
        fields.push(format!("foreverString = '{}'", escape_lua_string(s)));
    }
    if uploads {
        fields.push("foreverUpload = true".to_string());
    }
    fields.push(format!("writtenAt = {written_at}"));
    format!("GoldCap_AppData = {{ {} }}\n", fields.join(", "))
}

/// The Forever install's `GoldCap_AppData`: its `_Camelot.toc` (written only when it differs)
/// and `AppData.lua` (atomic). Nothing else: no retail toc, no LedgerSummary.lua, no Runs.lua.
// used from Task C5 on
#[allow(dead_code)]
pub fn write_forever_app_data(dir: &Path, interface: i64, forever_string: Option<&str>, uploads: bool, written_at: i64) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    let toc = dir.join(FOREVER_TOC_FILE_NAME);
    let want = forever_toc_contents(interface);
    if fs::read_to_string(&toc).ok().as_deref() != Some(want.as_str()) {
        fs::write(&toc, &want)?;
    }
    write_atomic(&dir.join(LUA_FILE_NAME), &render_forever_app_data_lua(forever_string, uploads, written_at))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "goldcap-companion-lua-{label}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn valid_gcs1_body_accepted() {
        assert!(is_valid_gcs1_body("GCS1;eu;dentarg;1752345678;..."));
    }

    #[test]
    fn non_gcs1_bodies_rejected() {
        assert!(!is_valid_gcs1_body(""));
        assert!(!is_valid_gcs1_body("realm_not_found"));
        assert!(!is_valid_gcs1_body("no_data"));
        assert!(!is_valid_gcs1_body("<html>502 Bad Gateway</html>"));
        assert!(!is_valid_gcs1_body("gcs1;lowercase-magic-does-not-count"));
    }

    #[test]
    fn escape_handles_backslash_and_quote() {
        assert_eq!(escape_lua_string(r"back\slash"), r"back\\slash");
        assert_eq!(escape_lua_string("it's"), r"it\'s");
        assert_eq!(escape_lua_string("a\nb"), r"a\nb");
        assert_eq!(
            escape_lua_string("plain-GCS1;eu:dentarg=1,2.3"),
            "plain-GCS1;eu:dentarg=1,2.3"
        );
    }

    #[test]
    fn escape_drops_nul_bytes_defensively() {
        assert_eq!(escape_lua_string("a\0b"), "ab");
    }

    #[test]
    fn render_app_data_lua_matches_expected_shape() {
        let rendered = render_app_data_lua("GCS1;eu;dentarg;1;abc", None, 1_752_345_678);
        assert_eq!(
            rendered,
            "GoldCap_AppData = { importString = 'GCS1;eu;dentarg;1;abc', writtenAt = 1752345678 }\n"
        );
    }

    #[test]
    fn render_app_data_lua_carries_the_region_string_between_the_two() {
        let rendered = render_app_data_lua(
            "GCS1;eu;dentarg;1;abc",
            Some("GCM1;eu;1789819200;I:1=2"),
            42,
        );
        assert_eq!(
            rendered,
            "GoldCap_AppData = { importString = 'GCS1;eu;dentarg;1;abc', regionString = 'GCM1;eu;1789819200;I:1=2', writtenAt = 42 }\n"
        );
    }

    #[test]
    fn render_app_data_lua_escapes_embedded_quote() {
        let rendered = render_app_data_lua("GCS1;it's-fine", None, 1);
        assert!(rendered.contains(r"GCS1;it\'s-fine"));
    }

    #[test]
    fn render_app_data_lua_escapes_the_region_string_too() {
        // Quote, backslash, both line breaks and a NUL: the NUL is dropped, the rest escaped.
        let rendered = render_app_data_lua("GCS1;a", Some("GCM1;it's\\\n\r\0fine"), 1);
        assert!(
            rendered.contains(r"regionString = 'GCM1;it\'s\\\n\rfine'"),
            "{rendered}"
        );
    }

    #[test]
    fn write_atomic_produces_final_file_with_no_tmp_left_behind() {
        let dir = temp_dir("atomic");
        fs::create_dir_all(&dir).unwrap();
        let target = dir.join("AppData.lua");

        write_atomic(&target, "hello").unwrap();

        assert_eq!(fs::read_to_string(&target).unwrap(), "hello");
        assert!(
            !tmp_path_for(&target).exists(),
            "the .tmp file must be renamed away, not left behind"
        );

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn write_atomic_overwrites_existing_file() {
        let dir = temp_dir("atomic-overwrite");
        fs::create_dir_all(&dir).unwrap();
        let target = dir.join("AppData.lua");
        fs::write(&target, "old contents").unwrap();

        write_atomic(&target, "new contents").unwrap();

        assert_eq!(fs::read_to_string(&target).unwrap(), "new contents");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn ensure_toc_writes_when_missing() {
        let dir = temp_dir("toc-missing");
        let wrote = ensure_toc(&dir).unwrap();
        assert!(wrote);
        assert_eq!(
            fs::read_to_string(dir.join(TOC_FILE_NAME)).unwrap(),
            TOC_CONTENTS
        );
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn ensure_toc_is_a_no_op_when_content_already_matches() {
        let dir = temp_dir("toc-match");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(TOC_FILE_NAME), TOC_CONTENTS).unwrap();

        let wrote = ensure_toc(&dir).unwrap();
        assert!(!wrote);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn ensure_toc_rewrites_when_content_differs() {
        let dir = temp_dir("toc-stale");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(TOC_FILE_NAME), "## Interface: 110000\nold\n").unwrap();

        let wrote = ensure_toc(&dir).unwrap();
        assert!(wrote);
        assert_eq!(
            fs::read_to_string(dir.join(TOC_FILE_NAME)).unwrap(),
            TOC_CONTENTS
        );
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn write_app_data_lua_writes_expected_file() {
        let dir = temp_dir("appdata");
        fs::create_dir_all(&dir).unwrap();
        write_app_data_lua(&dir, "GCS1;eu;dentarg;1;abc", None, 42).unwrap();

        let contents = fs::read_to_string(dir.join(LUA_FILE_NAME)).unwrap();
        assert_eq!(
            contents,
            "GoldCap_AppData = { importString = 'GCS1;eu;dentarg;1;abc', writtenAt = 42 }\n"
        );
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn write_app_data_lua_writes_the_region_string_atomically() {
        let dir = temp_dir("appdata-region");
        fs::create_dir_all(&dir).unwrap();
        write_app_data_lua(&dir, "GCS1;eu;dentarg;1;abc", Some("GCM1;eu;1;I:1=2"), 42).unwrap();

        let contents = fs::read_to_string(dir.join(LUA_FILE_NAME)).unwrap();
        assert!(contents.contains("regionString = 'GCM1;eu;1;I:1=2'"));
        assert!(!tmp_path_for(&dir.join(LUA_FILE_NAME)).exists());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn remove_ledger_summary_deletes_existing_file() {
        let dir = temp_dir("ledger-remove");
        fs::create_dir_all(&dir).unwrap();
        let target = dir.join(LEDGER_FILE_NAME);
        fs::write(&target, "GoldCapLedgerSummary = {}\n").unwrap();

        remove_ledger_summary(&dir).unwrap();

        assert!(!target.exists());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn remove_ledger_summary_is_ok_when_already_gone() {
        let dir = temp_dir("ledger-remove-twice");
        fs::create_dir_all(&dir).unwrap();
        let target = dir.join(LEDGER_FILE_NAME);
        fs::write(&target, "GoldCapLedgerSummary = {}\n").unwrap();

        remove_ledger_summary(&dir).unwrap();
        remove_ledger_summary(&dir).unwrap();

        assert!(!target.exists());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn remove_runs_deletes_existing_file() {
        let dir = temp_dir("runs-remove");
        fs::create_dir_all(&dir).unwrap();
        let target = dir.join(RUNS_FILE_NAME);
        fs::write(&target, "GoldCap_AppRuns = {}\n").unwrap();

        remove_runs(&dir).unwrap();

        assert!(!target.exists());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn remove_runs_is_ok_when_already_gone() {
        let dir = temp_dir("runs-remove-twice");
        fs::create_dir_all(&dir).unwrap();
        let target = dir.join(RUNS_FILE_NAME);
        fs::write(&target, "GoldCap_AppRuns = {}\n").unwrap();

        remove_runs(&dir).unwrap();
        remove_runs(&dir).unwrap();

        assert!(!target.exists());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn addon_dir_builds_expected_relative_path() {
        let root = Path::new("/Applications/World of Warcraft/_retail_");
        let dir = addon_dir(root);
        assert_eq!(
            dir,
            PathBuf::from(
                "/Applications/World of Warcraft/_retail_/Interface/AddOns/GoldCap_AppData"
            )
        );
    }

    #[test]
    fn a_forever_install_gets_its_own_toc_and_nothing_retail() {
        let dir = temp_dir("forever-appdata");
        write_forever_app_data(&dir, 16001, Some("GCF1;s;90;R;Horde;1;I:1=2=2===3=1=0"), true, 1790000000).unwrap();
        let mut names: Vec<String> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        assert_eq!(names, vec!["AppData.lua".to_string(), FOREVER_TOC_FILE_NAME.to_string()]);
        assert!(std::fs::read_to_string(dir.join(FOREVER_TOC_FILE_NAME)).unwrap().starts_with("## Interface: 16001\n"));
        assert_eq!(
            std::fs::read_to_string(dir.join(LUA_FILE_NAME)).unwrap(),
            "GoldCap_AppData = { foreverString = 'GCF1;s;90;R;Horde;1;I:1=2=2===3=1=0', foreverUpload = true, writtenAt = 1790000000 }\n"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_forever_install_without_prices_or_pairing_still_says_the_companion_is_there() {
        assert_eq!(render_forever_app_data_lua(None, false, 5), "GoldCap_AppData = { writtenAt = 5 }\n");
        assert_eq!(render_forever_app_data_lua(None, true, 5), "GoldCap_AppData = { foreverUpload = true, writtenAt = 5 }\n");
        // A realm with a quote survives the Lua string.
        assert_eq!(
            render_forever_app_data_lua(Some("GCF1;s;90;Zul'jin;-;1;I:1=2=2===3=1=0"), false, 5),
            "GoldCap_AppData = { foreverString = 'GCF1;s;90;Zul\\'jin;-;1;I:1=2=2===3=1=0', writtenAt = 5 }\n"
        );
    }

    #[test]
    fn retail_appdata_is_byte_for_byte_what_it_was() {
        // The two shapes retail has ever written — this plan must not move a byte of either.
        assert_eq!(render_app_data_lua("GCS1;eu;x;1;I:1=2", None, 7), "GoldCap_AppData = { importString = 'GCS1;eu;x;1;I:1=2', writtenAt = 7 }\n");
        assert_eq!(
            render_app_data_lua("GCS1;eu;x;1;I:1=2", Some("GCM1;eu;1;I:1=2"), 7),
            "GoldCap_AppData = { importString = 'GCS1;eu;x;1;I:1=2', regionString = 'GCM1;eu;1;I:1=2', writtenAt = 7 }\n"
        );
        assert!(!TOC_CONTENTS.contains("16001"), "retail's toc is retail's");
    }
}
