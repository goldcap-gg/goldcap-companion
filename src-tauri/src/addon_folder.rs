//! Who looks after a `GoldCap` addon folder, and what to do about it. The
//! Companion only ever touches a folder it installed itself (it left a marker
//! whose version agrees with the TOC) or one the player handed over.

use crate::addon_manifest::Version;
use serde::{Deserialize, Serialize};
use std::io;
use std::path::Path;

pub const MARKER_NAME: &str = ".goldcap-companion.json";

const TOC_NAMES: [&str; 2] = ["GoldCap.toc", "GoldCap_Camelot.toc"];

/// Left inside a folder the Companion installed. `installed_at` is unix seconds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Marker {
    pub version: String,
    pub installed_at: i64,
}

pub fn write_marker(dir: &Path, version: &Version, now: i64) -> io::Result<()> {
    let marker = Marker { version: version.to_string(), installed_at: now };
    let json = serde_json::to_string(&marker).map_err(io::Error::other)?;
    crate::luafile::write_atomic(&dir.join(MARKER_NAME), &json)
}

/// The `## Version:` of the retail TOC, else of the Forever one.
pub fn toc_version(addon_dir: &Path) -> Option<Version> {
    TOC_NAMES.iter().find_map(|name| {
        let text = std::fs::read_to_string(addon_dir.join(name)).ok()?;
        text.lines().find_map(|line| {
            let rest = line.trim_start().strip_prefix("##")?;
            let value = rest.trim_start().strip_prefix("Version:")?;
            Version::parse(value)
        })
    })
}

#[derive(Debug, Clone, PartialEq)]
pub enum FolderState {
    Missing,
    /// A real folder with our marker, whose version is the TOC's.
    Ours(Version),
    /// Anything else, with the TOC's version when it has a readable one.
    Foreign(Option<Version>),
}

/// Reads `addons_dir/GoldCap` without following a symlink.
pub fn inspect(addons_dir: &Path) -> FolderState {
    let dir = addons_dir.join("GoldCap");
    let meta = match std::fs::symlink_metadata(&dir) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return FolderState::Missing,
        Err(_) => return FolderState::Foreign(None),
    };
    if !meta.is_dir() {
        return FolderState::Foreign(None);
    }
    let toc = toc_version(&dir);
    let marker = std::fs::read_to_string(dir.join(MARKER_NAME))
        .ok()
        .and_then(|t| serde_json::from_str::<Marker>(&t).ok())
        .and_then(|m| Version::parse(&m.version));
    match (marker, toc) {
        (Some(m), Some(t)) if m == t => FolderState::Ours(t),
        _ => FolderState::Foreign(toc),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Install,
    Update,
    UpToDate,
    LeaveAlone,
    TakenOverWaiting,
}

pub fn decide(state: &FolderState, taken_over: bool, available: &Version) -> Decision {
    match state {
        FolderState::Missing => Decision::Install,
        FolderState::Ours(v) if v < available => Decision::Update,
        FolderState::Ours(_) => Decision::UpToDate,
        FolderState::Foreign(Some(v)) if taken_over && v < available => Decision::Update,
        FolderState::Foreign(Some(_)) if taken_over => Decision::TakenOverWaiting,
        FolderState::Foreign(_) => Decision::LeaveAlone,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;

    fn v(a: u32, b: u32, c: u32) -> Version {
        Version(a, b, c)
    }

    fn goldcap(addons: &Path) -> std::path::PathBuf {
        let d = addons.join("GoldCap");
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn toc(dir: &Path, name: &str, version: &str) {
        fs::write(dir.join(name), format!("## Title: GoldCap\n## Version: {version}\n")).unwrap();
    }

    // The decision table, one test per row.

    #[test]
    fn missing_installs() {
        for taken in [false, true] {
            assert_eq!(decide(&FolderState::Missing, taken, &v(0, 23, 0)), Decision::Install);
        }
    }

    #[test]
    fn ours_older_updates() {
        for taken in [false, true] {
            assert_eq!(decide(&FolderState::Ours(v(0, 22, 0)), taken, &v(0, 23, 0)), Decision::Update);
        }
    }

    #[test]
    fn ours_same_or_newer_is_up_to_date() {
        for installed in [v(0, 23, 0), v(0, 24, 0)] {
            assert_eq!(decide(&FolderState::Ours(installed), false, &v(0, 23, 0)), Decision::UpToDate);
        }
    }

    #[test]
    fn foreign_taken_over_older_updates() {
        assert_eq!(decide(&FolderState::Foreign(Some(v(0, 22, 0))), true, &v(0, 23, 0)), Decision::Update);
    }

    #[test]
    fn foreign_taken_over_current_waits() {
        for installed in [v(0, 23, 0), v(0, 24, 0)] {
            assert_eq!(
                decide(&FolderState::Foreign(Some(installed)), true, &v(0, 23, 0)),
                Decision::TakenOverWaiting
            );
        }
    }

    #[test]
    fn foreign_not_taken_over_is_left_alone() {
        assert_eq!(decide(&FolderState::Foreign(Some(v(0, 20, 0))), false, &v(0, 23, 0)), Decision::LeaveAlone);
    }

    #[test]
    fn foreign_without_a_version_is_left_alone_even_when_taken_over() {
        for taken in [false, true] {
            assert_eq!(decide(&FolderState::Foreign(None), taken, &v(0, 23, 0)), Decision::LeaveAlone);
        }
    }

    // The filesystem.

    #[test]
    fn missing_when_nothing_is_there() {
        let t = tempfile::tempdir().unwrap();
        assert_eq!(inspect(t.path()), FolderState::Missing);
    }

    #[test]
    fn ours_needs_marker_and_matching_toc() {
        let t = tempfile::tempdir().unwrap();
        let d = goldcap(t.path());
        toc(&d, "GoldCap.toc", "addon-v0.22.0");
        write_marker(&d, &v(0, 22, 0), 1_700_000_000).unwrap();
        assert_eq!(inspect(t.path()), FolderState::Ours(v(0, 22, 0)));
        let m: Marker = serde_json::from_str(&fs::read_to_string(d.join(MARKER_NAME)).unwrap()).unwrap();
        assert_eq!(m, Marker { version: "0.22.0".into(), installed_at: 1_700_000_000 });
    }

    #[test]
    fn no_marker_is_foreign() {
        let t = tempfile::tempdir().unwrap();
        toc(&goldcap(t.path()), "GoldCap.toc", "addon-v0.20.0");
        assert_eq!(inspect(t.path()), FolderState::Foreign(Some(v(0, 20, 0))));
    }

    #[test]
    fn a_marker_that_disagrees_with_the_toc_is_foreign() {
        let t = tempfile::tempdir().unwrap();
        let d = goldcap(t.path());
        write_marker(&d, &v(0, 21, 0), 1).unwrap();
        toc(&d, "GoldCap.toc", "addon-v0.22.0");
        assert_eq!(inspect(t.path()), FolderState::Foreign(Some(v(0, 22, 0))));
    }

    #[test]
    fn a_corrupt_marker_is_foreign() {
        let t = tempfile::tempdir().unwrap();
        let d = goldcap(t.path());
        toc(&d, "GoldCap.toc", "addon-v0.22.0");
        fs::write(d.join(MARKER_NAME), "{not json").unwrap();
        assert_eq!(inspect(t.path()), FolderState::Foreign(Some(v(0, 22, 0))));
    }

    #[test]
    fn a_dev_checkout_toc_is_foreign_with_no_version() {
        let t = tempfile::tempdir().unwrap();
        let d = goldcap(t.path());
        toc(&d, "GoldCap.toc", "@project-version@");
        write_marker(&d, &v(0, 22, 0), 1).unwrap();
        assert_eq!(inspect(t.path()), FolderState::Foreign(None));
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_goldcap_is_foreign() {
        let t = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        toc(other.path(), "GoldCap.toc", "addon-v0.22.0");
        write_marker(other.path(), &v(0, 22, 0), 1).unwrap();
        std::os::unix::fs::symlink(other.path(), t.path().join("GoldCap")).unwrap();
        assert!(matches!(inspect(t.path()), FolderState::Foreign(_)));
    }

    #[test]
    fn a_plain_file_named_goldcap_is_foreign() {
        let t = tempfile::tempdir().unwrap();
        fs::write(t.path().join("GoldCap"), "x").unwrap();
        assert_eq!(inspect(t.path()), FolderState::Foreign(None));
    }

    #[test]
    fn toc_version_falls_back_to_the_camelot_toc() {
        let t = tempfile::tempdir().unwrap();
        toc(t.path(), "GoldCap_Camelot.toc", "addon-v0.22.0");
        assert_eq!(toc_version(t.path()), Some(v(0, 22, 0)));
        toc(t.path(), "GoldCap.toc", "addon-v0.21.0");
        assert_eq!(toc_version(t.path()), Some(v(0, 21, 0)));
    }
}
