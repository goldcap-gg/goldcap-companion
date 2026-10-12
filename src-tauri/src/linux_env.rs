//! What the companion sets in its own environment on Linux, and takes back out of the
//! environment of the programs it starts: the browser it opens for pairing or the downloads page.

use std::ffi::{OsStr, OsString};
use std::process::Command;
use std::sync::OnceLock;

/// The variables `prepare` set itself. A program the companion starts must not inherit them.
static OURS: OnceLock<Vec<&'static str>> = OnceLock::new();

/// What the AppImage's start script (linuxdeploy's GTK hook) exports so GTK loads its theme,
/// modules and data from inside the AppImage. They mean nothing to another program, and they
/// point into a mount that goes away when the companion quits.
const APPIMAGE_GTK_VARS: [&str; 9] = [
    "GTK_THEME",
    "GTK_DATA_PREFIX",
    "GTK_EXE_PREFIX",
    "GTK_PATH",
    "GTK_IM_MODULE_FILE",
    "GDK_PIXBUF_MODULE_FILE",
    "GSETTINGS_SCHEMA_DIR",
    "GI_TYPELIB_PATH",
    "GIO_MODULE_DIR",
];

/// Runs first thing in `main`, before any thread exists to read the environment.
pub fn prepare() {
    let mut ours = Vec::new();
    // WebKitGTK's DMA-BUF renderer draws a blank window on many NVIDIA drivers, which is a
    // common Linux gaming setup. The companion's windows are a few forms, so the slower path
    // costs nothing anyone sees.
    if std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none() {
        std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
        ours.push("WEBKIT_DISABLE_DMABUF_RENDERER");
    }
    // The page is dark whatever the desktop is set to, but what GTK draws for it (a dropdown's
    // open list, the folder picker) follows the desktop: Ubuntu's light theme put a white list
    // over the dark window. Adwaita ships with GTK itself, so its dark variant is there on every
    // distribution. Only GTK_THEME reaches that list: setting the same theme on the window's own
    // GTK settings left it white.
    if !players_own_theme(|name| std::env::var_os(name)) {
        std::env::set_var("GTK_THEME", "Adwaita:dark");
        ours.push("GTK_THEME");
    }
    let _ = OURS.set(ours);
}

/// Whether GTK_THEME is one the player chose. In the AppImage it never is: the start script
/// sets it on every launch from the desktop's light or dark setting, and the player's own
/// choice goes in APPIMAGE_GTK_THEME, which the script copies over.
fn players_own_theme(var: impl Fn(&str) -> Option<OsString>) -> bool {
    if var("APPIMAGE").is_some() {
        var("APPIMAGE_GTK_THEME").is_some()
    } else {
        var("GTK_THEME").is_some()
    }
}

/// Starts `cmd` with neither what `prepare` set nor, in the AppImage, what its start script
/// pointed at the AppImage.
pub fn for_child(cmd: &mut Command) {
    for name in OURS.get().into_iter().flatten() {
        cmd.env_remove(name);
    }
    let appdir = std::env::var_os("APPDIR").filter(|_| std::env::var_os("APPIMAGE").is_some());
    if let Some(appdir) = appdir {
        for name in APPIMAGE_GTK_VARS {
            cmd.env_remove(name);
        }
        // The script put the AppImage's own folder in front of the session's list; the rest,
        // Flatpak's exports among them, is how the browser's launcher gets found.
        if let Some(dirs) = std::env::var_os("XDG_DATA_DIRS") {
            cmd.env("XDG_DATA_DIRS", without_dirs_under(&dirs, &appdir));
        }
    }
}

/// A `:`-separated folder list without the folders inside `root`.
fn without_dirs_under(list: &OsStr, root: &OsStr) -> OsString {
    let root = root.to_string_lossy();
    let root = root.trim_end_matches('/');
    let kept: Vec<String> = list
        .to_string_lossy()
        .split(':')
        .filter(|dir| *dir != root && !dir.starts_with(&format!("{root}/")))
        .map(str::to_string)
        .collect();
    OsString::from(kept.join(":"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<OsString> {
        move |name| {
            pairs
                .iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| OsString::from(v))
        }
    }

    #[test]
    fn a_theme_set_outside_the_appimage_is_the_players() {
        assert!(players_own_theme(env(&[("GTK_THEME", "Breeze:dark")])));
        assert!(!players_own_theme(env(&[])));
    }

    #[test]
    fn in_the_appimage_only_its_own_override_is_the_players() {
        // The start script always sets GTK_THEME, light on a light desktop.
        let script_only = env(&[("APPIMAGE", "/home/u/GoldCap.AppImage"), ("GTK_THEME", "Adwaita:light")]);
        assert!(!players_own_theme(script_only));
        let overridden = env(&[
            ("APPIMAGE", "/home/u/GoldCap.AppImage"),
            ("APPIMAGE_GTK_THEME", "Breeze"),
            ("GTK_THEME", "Breeze"),
        ]);
        assert!(players_own_theme(overridden));
    }

    #[test]
    fn the_appimages_folders_leave_the_data_dirs_and_the_sessions_stay() {
        let list = OsStr::new(
            "/tmp/.mount_GoldCaX/usr/share:/usr/share:/var/lib/flatpak/exports/share:/tmp/.mount_GoldCaXY/share",
        );
        assert_eq!(
            without_dirs_under(list, OsStr::new("/tmp/.mount_GoldCaX/")),
            OsString::from("/usr/share:/var/lib/flatpak/exports/share:/tmp/.mount_GoldCaXY/share")
        );
    }
}
