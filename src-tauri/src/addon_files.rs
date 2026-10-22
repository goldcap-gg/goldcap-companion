//! Putting an addon release on disk: a hostile-zip-safe unpack into a staging
//! folder beside `AddOns`, a swap by rename that rolls back on any failure, and
//! the cleanup of what a crash left behind. Nothing here touches anything but
//! `GoldCap`, `GoldCap.old-*` and `.goldcap-staging-*` inside the folder it is
//! given.

use crate::addon_folder::{write_marker, MARKER_NAME};
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
    MoveOldAside,
    MoveNewIn,
    WriteMarker,
    DeleteOld,
}

/// `staged_top` is `<staging>/GoldCap`. Old folder aside, new one in, marker,
/// old one deleted; any failure before the delete puts everything back.
pub fn swap_in(addons_dir: &Path, staged_top: &Path, version: &Version, now: i64) -> Result<(), String> {
    swap_steps(addons_dir, staged_top, version, now, &|_| Ok(()))
}

fn swap_steps(
    addons_dir: &Path,
    staged_top: &Path,
    version: &Version,
    now: i64,
    hook: &dyn Fn(SwapStep) -> io::Result<()>,
) -> Result<(), String> {
    let live = addons_dir.join(TOP);
    let aside = addons_dir.join(format!("{OLD_PREFIX}{now}"));
    let had_old = fs::symlink_metadata(&live).is_ok();

    if had_old {
        hook(SwapStep::MoveOldAside)
            .and_then(|_| fs::rename(&live, &aside))
            .map_err(|e| format!("could not move the old addon aside: {e}"))?;
    }
    let rollback = |new_in: bool| {
        if new_in {
            let _ = fs::remove_dir_all(&live);
        }
        if had_old {
            let _ = fs::rename(&aside, &live);
        }
    };
    if let Err(e) = hook(SwapStep::MoveNewIn).and_then(|_| fs::rename(staged_top, &live)) {
        rollback(false);
        return Err(format!("could not move the new addon in: {e}"));
    }
    if let Err(e) = hook(SwapStep::WriteMarker).and_then(|_| write_marker(&live, version, now)) {
        rollback(true);
        return Err(format!("could not write the marker: {e}"));
    }
    // The swap has succeeded; a folder that cannot be deleted now is a
    // leftover the next `recover` removes.
    if had_old {
        let _ = hook(SwapStep::DeleteOld).and_then(|_| fs::remove_dir_all(&aside));
    }
    Ok(())
}

/// After a crash: restore a `GoldCap` moved aside, then delete leftovers.
pub fn recover(addons_dir: &Path) {
    let Ok(read) = fs::read_dir(addons_dir) else { return };
    let mut olds: Vec<(i64, PathBuf)> = Vec::new();
    let mut stagings: Vec<PathBuf> = Vec::new();
    for entry in read.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let path = entry.path();
        if let Some(ts) = name.strip_prefix(OLD_PREFIX) {
            olds.push((ts.parse().unwrap_or(0), path));
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
    if fs::symlink_metadata(&live).is_ok() {
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
    use crate::addon_folder::{inspect, FolderState};
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
        assert!(!addons.join("GoldCap.old-100").exists());
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
            assert!(!addons.join("GoldCap.old-100").exists(), "{failing:?}");
        }
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
        fs::rename(addons.join("GoldCap"), addons.join("GoldCap.old-50")).unwrap();
        recover(&addons);
        assert_eq!(inspect(&addons), FolderState::Ours(v(0, 22, 0)));
        assert!(!addons.join("GoldCap.old-50").exists());
        assert!(!staging_dir(&addons, 100).exists());
    }

    #[test]
    fn recover_restores_the_newest_when_there_are_several() {
        let (_t, addons) = setup();
        fs::rename(addons.join("GoldCap"), addons.join("GoldCap.old-90")).unwrap();
        fs::create_dir_all(addons.join("GoldCap.old-9")).unwrap();
        recover(&addons);
        assert_eq!(inspect(&addons), FolderState::Ours(v(0, 22, 0)));
        assert!(!addons.join("GoldCap.old-9").exists());
    }

    #[test]
    fn recover_deletes_leftovers_when_goldcap_exists() {
        let (_t, addons) = setup();
        fs::create_dir_all(addons.join("GoldCap.old-50")).unwrap();
        recover(&addons);
        assert!(!addons.join("GoldCap.old-50").exists());
        assert!(!staging_dir(&addons, 100).exists());
        assert!(addons.join("GoldCap/Core/old.lua").exists());
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
