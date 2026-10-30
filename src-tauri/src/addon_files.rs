//! Putting an addon release on disk: a hostile-zip-safe unpack into a staging
//! folder beside `AddOns`, a swap by rename that rolls back on any failure, and
//! the cleanup of what a crash left behind. Nothing here touches anything but
//! `GoldCap`, the `GoldCap.old-<ten digits>` a swap makes and
//! `.goldcap-staging-*` inside the folder it is given.

use crate::addon_folder::{inspect, write_marker, FolderState, MARKER_NAME};
use crate::addon_manifest::Version;
use std::fs;
use std::io::{self, Cursor, Read};
use std::path::{Component, Path, PathBuf};

pub const MAX_ENTRIES: usize = 4000;
pub const MAX_UNPACKED_BYTES: u64 = 64 * 1024 * 1024;

const TOP: &str = "GoldCap";
const APPDATA: &str = "GoldCap_AppData";
const OLD_PREFIX: &str = "GoldCap.old-";
const STAGING_PREFIX: &str = ".goldcap-staging-";
const EXECUTABLE_EXTENSIONS: [&str; 11] =
    ["exe", "dll", "bat", "cmd", "com", "scr", "ps1", "sh", "msi", "jar", "vbs"];

/// Same filesystem as `AddOns`, so the swap's renames never cross a volume.
pub fn staging_dir(addons_dir: &Path, now: i64) -> PathBuf {
    addons_dir.join(format!("{STAGING_PREFIX}{now}"))
}

/// Where a swap moves the old `GoldCap`: the prefix and the time as exactly
/// ten digits. Only a name of this form is the Companion's own backup.
pub fn aside_dir(addons_dir: &Path, now: i64) -> PathBuf {
    addons_dir.join(format!("{OLD_PREFIX}{:010}", now.clamp(0, 9_999_999_999)))
}

/// The time in a name `aside_dir` makes; `None` for anything else, such as a
/// player's own `GoldCap.old-backup`.
fn aside_time(name: &str) -> Option<i64> {
    let ts = name.strip_prefix(OLD_PREFIX)?;
    (ts.len() == 10 && ts.bytes().all(|b| b.is_ascii_digit())).then(|| ts.parse().ok())?
}

fn is_executable_name(name: &str) -> bool {
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    ext == "js" || EXECUTABLE_EXTENSIONS.contains(&ext.as_str())
}

/// Unpacks `zip` into `dest` (an empty folder the caller made) so that
/// `dest/GoldCap/...` exists. On `Err` the caller deletes `dest`.
pub fn unpack(zip: &[u8], dest: &Path) -> Result<(), String> {
    let mut archive =
        zip::ZipArchive::new(Cursor::new(zip)).map_err(|e| format!("not a readable zip: {e}"))?;
    if archive.len() > MAX_ENTRIES {
        return Err(format!("zip has more than {MAX_ENTRIES} entries"));
    }
    let mut budget = MAX_UNPACKED_BYTES;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| format!("zip entry {i}: {e}"))?;
        let raw_name = entry.name().to_string();
        let rel = entry
            .enclosed_name()
            .ok_or_else(|| format!("zip entry {raw_name:?} leaves the folder"))?;
        let parts: Vec<String> = rel
            .components()
            .map(|c| match c {
                Component::Normal(p) => Ok(p.to_string_lossy().to_string()),
                _ => Err(format!("zip entry {raw_name:?} has an unusual path")),
            })
            .collect::<Result<_, _>>()?;
        if parts.first().map(String::as_str) != Some(TOP) {
            return Err(format!("zip entry {raw_name:?} is outside the {TOP} folder"));
        }
        if entry.unix_mode().is_some_and(|m| m & 0o170000 == 0o120000) {
            return Err(format!("zip entry {raw_name:?} is a symbolic link"));
        }
        if parts.get(1).map(String::as_str) == Some(APPDATA) {
            return Err(format!("zip entry {raw_name:?} would overwrite {APPDATA}"));
        }
        if parts.iter().any(|p| p == MARKER_NAME) {
            return Err(format!("zip entry {raw_name:?} carries the Companion's own marker"));
        }
        let target = dest.join(&rel);
        if entry.is_dir() {
            fs::create_dir_all(&target).map_err(|e| format!("create {}: {e}", target.display()))?;
            continue;
        }
        if parts.len() < 2 {
            return Err(format!("zip entry {raw_name:?} is a file where the {TOP} folder belongs"));
        }
        if is_executable_name(parts.last().map(String::as_str).unwrap_or_default()) {
            return Err(format!("zip entry {raw_name:?} is an executable"));
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
        }
        let mut out =
            fs::File::create(&target).map_err(|e| format!("create {}: {e}", target.display()))?;
        // The budget is enforced while reading: the header's size is the zip's
        // own claim and a bomb lies in it.
        let copied = io::copy(&mut (&mut entry).take(budget + 1), &mut out)
            .map_err(|e| format!("write {}: {e}", target.display()))?;
        if copied > budget {
            return Err(format!("zip unpacks to more than {MAX_UNPACKED_BYTES} bytes"));
        }
        budget -= copied;
    }
    if !dest.join(TOP).join("GoldCap.toc").is_file() {
        return Err("zip has no GoldCap/GoldCap.toc".to_string());
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwapStep {
    WriteMarker,
    MoveOldAside,
    MoveNewIn,
    PutOldBack,
    DeleteOld,
}

/// Runs `f` up to `attempts` times, waiting `wait` between tries. A virus
/// scanner or an indexer holding a freshly unpacked file makes a Windows
/// rename fail for a moment.
fn retry<T>(attempts: u32, wait: std::time::Duration, mut f: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    let mut left = attempts.max(1);
    loop {
        match f() {
            Ok(v) => return Ok(v),
            Err(e) if left <= 1 => return Err(e),
            Err(_) => {
                left -= 1;
                std::thread::sleep(wait);
            }
        }
    }
}

fn rename_retry(from: &Path, to: &Path) -> io::Result<()> {
    let attempts = if cfg!(windows) { 5 } else { 1 };
    retry(attempts, std::time::Duration::from_millis(200), || fs::rename(from, to))
}

/// `staged_top` is `<staging>/GoldCap`. The marker goes into the staged folder
/// first, so after any crash `GoldCap` is either the old folder or the new one,
/// each with its marker. Then old aside, new in, old deleted; a failure before
/// the delete puts the old folder back. `hook` runs before each step: the
/// installer passes one that does nothing, tests fail a step through it.
pub fn swap_steps(
    addons_dir: &Path,
    staged_top: &Path,
    version: &Version,
    now: i64,
    hook: &dyn Fn(SwapStep) -> io::Result<()>,
) -> Result<(), String> {
    let live = addons_dir.join(TOP);
    let aside = aside_dir(addons_dir, now);
    let had_old = fs::symlink_metadata(&live).is_ok();

    hook(SwapStep::WriteMarker)
        .and_then(|_| write_marker(staged_top, version, now))
        .map_err(|e| format!("could not write the marker: {e}"))?;
    if had_old {
        hook(SwapStep::MoveOldAside)
            .and_then(|_| rename_retry(&live, &aside))
            .map_err(|e| format!("could not move the old addon aside: {e}"))?;
    }
    if let Err(e) = hook(SwapStep::MoveNewIn).and_then(|_| rename_retry(staged_top, &live)) {
        if had_old {
            let _ = hook(SwapStep::PutOldBack).and_then(|_| rename_retry(&aside, &live));
        }
        return Err(format!("could not move the new addon in: {e}"));
    }
    // The swap has succeeded; a folder that cannot be deleted now is a
    // leftover the next `recover` removes.
    if had_old {
        let _ = hook(SwapStep::DeleteOld).and_then(|_| fs::remove_dir_all(&aside));
    }
    Ok(())
}

/// After a crash: restore a `GoldCap` moved aside, then delete leftovers.
/// Only folders a swap made are touched, never another `GoldCap.old-...`.
pub fn recover(addons_dir: &Path) {
    let Ok(read) = fs::read_dir(addons_dir) else { return };
    let mut olds: Vec<(i64, PathBuf)> = Vec::new();
    let mut stagings: Vec<PathBuf> = Vec::new();
    for entry in read.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let path = entry.path();
        if name.starts_with(OLD_PREFIX) {
            if let Some(ts) = aside_time(&name).filter(|_| fs::symlink_metadata(&path).is_ok_and(|m| m.is_dir())) {
                olds.push((ts, path));
            }
        } else if name.starts_with(STAGING_PREFIX) && path.is_dir() {
            stagings.push(path);
        }
    }
    let live = addons_dir.join(TOP);
    if matches!(fs::symlink_metadata(&live), Err(e) if e.kind() == io::ErrorKind::NotFound) {
        olds.sort();
        if let Some((_, newest)) = olds.last() {
            if fs::rename(newest, &live).is_ok() {
                olds.pop();
            }
        }
    }
    // A backup is deleted only when the live folder is a complete copy of
    // ours: it may be the only one.
    if matches!(inspect(addons_dir), FolderState::Ours(_)) {
        for (_, p) in olds {
            let _ = fs::remove_dir_all(p);
        }
    }
    for p in stagings {
        let _ = fs::remove_dir_all(p);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::{Cursor, Write};
    use zip::write::SimpleFileOptions;
    use zip::{CompressionMethod, ZipWriter};

    fn v(a: u32, b: u32, c: u32) -> Version {
        Version(a, b, c)
    }

    fn opts() -> SimpleFileOptions {
        SimpleFileOptions::default().compression_method(CompressionMethod::Deflated)
    }

    fn make_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut w = ZipWriter::new(Cursor::new(Vec::new()));
        for (name, body) in entries {
            w.start_file(*name, opts()).unwrap();
            w.write_all(body).unwrap();
        }
        w.finish().unwrap().into_inner()
    }

    fn good() -> Vec<(&'static str, &'static [u8])> {
        vec![
            ("GoldCap/GoldCap.toc", b"## Version: addon-v0.23.0\n"),
            ("GoldCap/Core/a.lua", b"local a = 1\n"),
        ]
    }

    fn swap_in(addons_dir: &Path, staged_top: &Path, version: &Version, now: i64) -> Result<(), String> {
        swap_steps(addons_dir, staged_top, version, now, &|_| Ok(()))
    }

    fn unpack_to_temp(zip: &[u8]) -> (tempfile::TempDir, Result<(), String>) {
        let t = tempfile::tempdir().unwrap();
        let r = unpack(zip, t.path());
        (t, r)
    }

    #[test]
    fn unpacks_a_normal_release() {
        let mut w = ZipWriter::new(Cursor::new(Vec::new()));
        w.start_file("GoldCap/GoldCap.toc", opts()).unwrap();
        w.write_all(b"## Version: addon-v0.23.0\n").unwrap();
        w.start_file("GoldCap/Core/a.lua", opts()).unwrap();
        w.write_all(b"local a = 1\n").unwrap();
        w.start_file(
            "GoldCap/Media/x.tga",
            SimpleFileOptions::default().compression_method(CompressionMethod::Stored),
        )
        .unwrap();
        w.write_all(&[1, 2, 3]).unwrap();
        let zip = w.finish().unwrap().into_inner();
        let (t, r) = unpack_to_temp(&zip);
        r.unwrap();
        assert_eq!(fs::read(t.path().join("GoldCap/Core/a.lua")).unwrap(), b"local a = 1\n");
        assert_eq!(fs::read(t.path().join("GoldCap/Media/x.tga")).unwrap(), [1, 2, 3]);
    }

    fn refused(extra: (&str, &[u8])) {
        let mut e = good();
        e.push(extra);
        let (_t, r) = unpack_to_temp(&make_zip(&e));
        assert!(r.is_err(), "{} should be refused", extra.0);
    }

    #[test]
    fn refuses_dot_dot() {
        refused(("GoldCap/../evil.lua", b"x"));
    }

    #[test]
    fn refuses_absolute_path() {
        refused(("/etc/evil.lua", b"x"));
    }

    #[test]
    fn refuses_a_second_top_folder() {
        refused(("Details/Core.lua", b"x"));
    }

    #[test]
    fn refuses_a_symlink_entry() {
        let mut w = ZipWriter::new(Cursor::new(Vec::new()));
        w.start_file("GoldCap/GoldCap.toc", opts()).unwrap();
        w.write_all(b"x").unwrap();
        w.add_symlink("GoldCap/link", "/etc", opts()).unwrap();
        let (_t, r) = unpack_to_temp(&w.finish().unwrap().into_inner());
        assert!(r.is_err());
    }

    #[test]
    fn refuses_an_exe() {
        refused(("GoldCap/Tools/Run.EXE", b"MZ"));
        refused(("GoldCap/x.sh", b"#"));
    }

    #[test]
    fn refuses_bundled_appdata() {
        refused(("GoldCap/GoldCap_AppData/AppData.lua", b"x"));
    }

    #[test]
    fn refuses_a_bundled_marker() {
        refused(("GoldCap/.goldcap-companion.json", b"{}"));
    }

    #[test]
    fn refuses_a_zip_without_the_toc() {
        let (_t, r) = unpack_to_temp(&make_zip(&[("GoldCap/Core/a.lua", b"x")]));
        assert!(r.is_err());
    }

    #[test]
    fn refuses_too_many_entries() {
        let mut w = ZipWriter::new(Cursor::new(Vec::new()));
        w.start_file("GoldCap/GoldCap.toc", opts()).unwrap();
        for i in 0..MAX_ENTRIES {
            w.start_file(format!("GoldCap/f{i}.lua"), opts()).unwrap();
        }
        let (_t, r) = unpack_to_temp(&w.finish().unwrap().into_inner());
        assert!(r.unwrap_err().contains("entries"));
    }

    #[test]
    fn refuses_a_zip_bomb() {
        let mut w = ZipWriter::new(Cursor::new(Vec::new()));
        w.start_file("GoldCap/GoldCap.toc", opts()).unwrap();
        w.start_file("GoldCap/bomb.lua", opts()).unwrap();
        let chunk = vec![0u8; 1024 * 1024];
        for _ in 0..70 {
            w.write_all(&chunk).unwrap();
        }
        let zip = w.finish().unwrap().into_inner();
        assert!(zip.len() < 1024 * 1024);
        let (_t, r) = unpack_to_temp(&zip);
        assert!(r.unwrap_err().contains("unpacks to more"));
    }

    // Swapping.

    fn tree(dir: &Path) -> Vec<(String, Vec<u8>)> {
        fn walk(base: &Path, dir: &Path, out: &mut Vec<(String, Vec<u8>)>) {
            let mut entries: Vec<_> = fs::read_dir(dir).unwrap().map(|e| e.unwrap().path()).collect();
            entries.sort();
            for p in entries {
                let rel = p.strip_prefix(base).unwrap().to_string_lossy().to_string();
                if p.is_dir() {
                    out.push((format!("{rel}/"), Vec::new()));
                    walk(base, &p, out);
                } else {
                    out.push((rel, fs::read(&p).unwrap()));
                }
            }
        }
        let mut out = Vec::new();
        walk(dir, dir, &mut out);
        out
    }

    /// An `AddOns` with an installed-by-us 0.22.0 and a staged 0.23.0.
    fn setup() -> (tempfile::TempDir, std::path::PathBuf) {
        let t = tempfile::tempdir().unwrap();
        let addons = t.path().join("AddOns");
        let old = addons.join("GoldCap");
        fs::create_dir_all(old.join("Core")).unwrap();
        fs::write(old.join("GoldCap.toc"), "## Version: addon-v0.22.0\n").unwrap();
        fs::write(old.join("Core/old.lua"), "old").unwrap();
        crate::addon_folder::write_marker(&old, &v(0, 22, 0), 1).unwrap();
        fs::create_dir_all(addons.join("GoldCap_AppData")).unwrap();
        fs::write(addons.join("GoldCap_AppData/AppData.lua"), "appdata").unwrap();
        let staging = staging_dir(&addons, 100);
        unpack(&make_zip(&good()), &{
            fs::create_dir_all(&staging).unwrap();
            staging.clone()
        })
        .unwrap();
        (t, addons)
    }

    #[test]
    fn swap_replaces_the_folder_and_writes_the_marker() {
        let (_t, addons) = setup();
        let staged = staging_dir(&addons, 100).join("GoldCap");
        swap_in(&addons, &staged, &v(0, 23, 0), 100).unwrap();
        assert_eq!(inspect(&addons), FolderState::Ours(v(0, 23, 0)));
        assert!(!addons.join("GoldCap/Core/old.lua").exists());
        assert!(addons.join("GoldCap/Core/a.lua").exists());
        assert!(!aside_dir(&addons, 100).exists());
    }

    #[test]
    fn a_failure_at_each_swap_step_leaves_the_old_folder_exactly_as_it_was() {
        for failing in [SwapStep::MoveOldAside, SwapStep::MoveNewIn, SwapStep::WriteMarker] {
            let (_t, addons) = setup();
            let before = tree(&addons.join("GoldCap"));
            let staged = staging_dir(&addons, 100).join("GoldCap");
            let r = swap_steps(&addons, &staged, &v(0, 23, 0), 100, &|s| {
                if s == failing {
                    Err(io::Error::other("disk says no"))
                } else {
                    Ok(())
                }
            });
            assert!(r.unwrap_err().contains("disk says no"), "{failing:?}");
            assert_eq!(tree(&addons.join("GoldCap")), before, "{failing:?}");
            assert!(!aside_dir(&addons, 100).exists(), "{failing:?}");
        }
    }

    /// A crash leaves the disk as it was after a step. Rebuilds that state, runs
    /// `recover` and expects a whole folder with its marker.
    #[test]
    fn a_crash_after_each_swap_step_recovers_to_a_whole_marked_folder() {
        let new = FolderState::Ours(v(0, 23, 0));
        let old = FolderState::Ours(v(0, 22, 0));
        // (steps done before the crash, expected version after recover)
        for (done, expected) in [(1, &old), (2, &old), (3, &new), (4, &new)] {
            let (_t, addons) = setup();
            let staged = staging_dir(&addons, 100).join("GoldCap");
            let live = addons.join("GoldCap");
            let aside = aside_dir(&addons, 100);
            if done >= 1 {
                write_marker(&staged, &v(0, 23, 0), 100).unwrap();
            }
            if done >= 2 {
                fs::rename(&live, &aside).unwrap();
            }
            if done >= 3 {
                fs::rename(&staged, &live).unwrap();
            }
            if done >= 4 {
                fs::remove_dir_all(&aside).unwrap();
            }
            recover(&addons);
            assert_eq!(&inspect(&addons), expected, "crash after step {done}");
            assert!(!aside.exists(), "crash after step {done}");
            assert!(!staging_dir(&addons, 100).exists(), "crash after step {done}");
        }
    }

    #[test]
    fn recover_keeps_the_backup_when_the_live_folder_is_not_ours() {
        let (_t, addons) = setup();
        fs::rename(addons.join("GoldCap"), aside_dir(&addons, 50)).unwrap();
        fs::create_dir_all(addons.join("GoldCap/Core")).unwrap();
        fs::write(addons.join("GoldCap/Core/half.lua"), "x").unwrap();
        recover(&addons);
        assert!(aside_dir(&addons, 50).join("GoldCap.toc").exists());
        assert!(addons.join("GoldCap/Core/half.lua").exists());
    }

    #[test]
    fn the_new_folder_carries_its_marker_before_it_is_moved_in() {
        let (_t, addons) = setup();
        let staged = staging_dir(&addons, 100).join("GoldCap");
        let seen = std::cell::Cell::new(false);
        swap_steps(&addons, &staged, &v(0, 23, 0), 100, &|s| {
            if s == SwapStep::MoveOldAside {
                seen.set(staged.join(MARKER_NAME).is_file());
            }
            Ok(())
        })
        .unwrap();
        assert!(seen.get());
    }

    #[test]
    fn retry_tries_again_and_gives_up_after_the_last_attempt() {
        let n = std::cell::Cell::new(0);
        let r = retry(5, std::time::Duration::ZERO, || {
            n.set(n.get() + 1);
            if n.get() < 3 { Err(io::Error::other("busy")) } else { Ok(n.get()) }
        });
        assert_eq!(r.unwrap(), 3);
        n.set(0);
        let r: io::Result<()> = retry(4, std::time::Duration::ZERO, || {
            n.set(n.get() + 1);
            Err(io::Error::other("busy"))
        });
        assert!(r.is_err());
        assert_eq!(n.get(), 4);
    }

    #[test]
    fn a_failed_delete_of_the_old_folder_is_still_a_success() {
        let (_t, addons) = setup();
        let staged = staging_dir(&addons, 100).join("GoldCap");
        swap_steps(&addons, &staged, &v(0, 23, 0), 100, &|s| {
            if s == SwapStep::DeleteOld {
                Err(io::Error::other("busy"))
            } else {
                Ok(())
            }
        })
        .unwrap();
        assert_eq!(inspect(&addons), FolderState::Ours(v(0, 23, 0)));
    }

    #[test]
    fn recover_restores_a_folder_moved_aside_by_a_crash() {
        let (_t, addons) = setup();
        fs::rename(addons.join("GoldCap"), aside_dir(&addons, 50)).unwrap();
        recover(&addons);
        assert_eq!(inspect(&addons), FolderState::Ours(v(0, 22, 0)));
        assert!(!aside_dir(&addons, 50).exists());
        assert!(!staging_dir(&addons, 100).exists());
    }

    #[test]
    fn recover_restores_the_newest_when_there_are_several() {
        let (_t, addons) = setup();
        fs::rename(addons.join("GoldCap"), aside_dir(&addons, 90)).unwrap();
        fs::create_dir_all(aside_dir(&addons, 9)).unwrap();
        recover(&addons);
        assert_eq!(inspect(&addons), FolderState::Ours(v(0, 22, 0)));
        assert!(!aside_dir(&addons, 9).exists());
    }

    #[test]
    fn recover_deletes_leftovers_when_goldcap_exists() {
        let (_t, addons) = setup();
        fs::create_dir_all(aside_dir(&addons, 50)).unwrap();
        recover(&addons);
        assert!(!aside_dir(&addons, 50).exists());
        assert!(!staging_dir(&addons, 100).exists());
        assert!(addons.join("GoldCap/Core/old.lua").exists());
    }

    #[test]
    fn recover_never_touches_a_players_own_backup() {
        let theirs = [
            "GoldCap.old-backup",
            "GoldCap.old-2024",
            "GoldCap.old-1",
            "GoldCap.old-17000000000",
            "GoldCap.old-1700000000-mine",
            "GoldCap.old-",
        ];
        let (_t, addons) = setup();
        for name in theirs {
            fs::create_dir_all(addons.join(name)).unwrap();
            fs::write(addons.join(name).join("keep.lua"), "mine").unwrap();
        }
        // Next to a folder the Companion installed, where its own backups are deleted.
        recover(&addons);
        // And with no `GoldCap` at all, where its own backup would be put back.
        fs::remove_dir_all(addons.join("GoldCap")).unwrap();
        recover(&addons);
        assert!(!addons.join("GoldCap").exists());
        for name in theirs {
            assert_eq!(fs::read(addons.join(name).join("keep.lua")).unwrap(), b"mine", "{name}");
        }
    }

    #[test]
    fn recover_does_nothing_to_unrelated_folders() {
        let (_t, addons) = setup();
        fs::create_dir_all(addons.join("Details")).unwrap();
        fs::create_dir_all(addons.join("GoldCapital.old-1")).unwrap();
        fs::remove_dir_all(addons.join("GoldCap")).unwrap();
        recover(&addons);
        assert!(addons.join("GoldCap_AppData/AppData.lua").exists());
        assert!(addons.join("Details").exists());
        assert!(addons.join("GoldCapital.old-1").exists());
        assert!(!addons.join("GoldCap").exists());
    }

    #[test]
    fn a_swap_never_touches_goldcap_appdata_or_wtf() {
        let (_t, addons) = setup();
        let staged = staging_dir(&addons, 100).join("GoldCap");
        swap_in(&addons, &staged, &v(0, 23, 0), 100).unwrap();
        assert_eq!(fs::read(addons.join("GoldCap_AppData/AppData.lua")).unwrap(), b"appdata");
    }

    #[test]
    fn a_first_install_swaps_into_an_empty_addons_folder() {
        let (_t, addons) = setup();
        fs::remove_dir_all(addons.join("GoldCap")).unwrap();
        let staged = staging_dir(&addons, 100).join("GoldCap");
        swap_in(&addons, &staged, &v(0, 23, 0), 100).unwrap();
        assert_eq!(inspect(&addons), FolderState::Ours(v(0, 23, 0)));
    }
}
