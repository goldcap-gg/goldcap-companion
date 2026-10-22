//! Whether World of Warcraft is running, behind a trait so the install logic
//! can be tested with a fake process list. Only std is used: no
//! process-listing crate. A wrong answer has to err towards "running": an
//! update applied under a running game can leave it half old, half new.

use std::io;
use std::path::Path;
use std::process::Command;

/// Executable base names (lowercase) of every WoW client: retail, Forever
/// (beta, test) and the macOS app's own binary.
pub const WOW_PROCESS_NAMES: &[&str] = &[
    "wow.exe",
    "wowb.exe",
    "wowt.exe",
    "wowclassic.exe",
    "wowclassicb.exe",
    "wowclassict.exe",
    "wow-arm64.exe",
    "wowb-arm64.exe",
    "wowt-arm64.exe",
    "wowclassic-arm64.exe",
    "wowclassicb-arm64.exe",
    "wowclassict-arm64.exe",
    "world of warcraft",
    "world of warcraft classic",
];

pub trait GameProcesses: Sync {
    /// Process executable names or paths, one per process. On Linux each line
    /// also carries the command line, which is where a Wine client shows its exe.
    fn executables(&self) -> io::Result<Vec<String>>;

    /// Why a client runs from `game_dir` whatever its name, or `None`. Only
    /// Windows can tell: elsewhere file locks are advisory.
    fn exe_in_use(&self, _game_dir: &Path) -> Option<String> {
        None
    }
}

pub struct SystemProcesses;

impl GameProcesses for SystemProcesses {
    fn executables(&self) -> io::Result<Vec<String>> {
        #[cfg(windows)]
        let output = {
            use std::os::windows::process::CommandExt;
            // CREATE_NO_WINDOW: a tray app must not flash a console every tick.
            Command::new("tasklist")
                .args(["/FO", "CSV", "/NH"])
                .creation_flags(0x0800_0000)
                .output()?
        };
        // macOS gives `comm` as the full path. On Linux `comm` is cut at 15
        // characters and some Wine launchers leave it as `wine64-preloade`, so
        // the command line goes along with it.
        #[cfg(target_os = "macos")]
        let output = Command::new("ps").args(["-A", "-o", "comm="]).output()?;
        #[cfg(not(any(windows, target_os = "macos")))]
        let output = Command::new("ps").args(["-A", "-ww", "-o", "comm=", "-o", "args="]).output()?;
        if !output.status.success() {
            return Err(io::Error::other(format!("process listing exited with {}", output.status)));
        }
        let text = String::from_utf8_lossy(&output.stdout);
        #[cfg(windows)]
        return Ok(parse_tasklist(&text));
        #[cfg(not(windows))]
        return Ok(text.lines().map(str::to_string).collect());
    }

    fn exe_in_use(&self, game_dir: &Path) -> Option<String> {
        #[cfg(windows)]
        return held_exe(game_dir, |p| std::fs::OpenOptions::new().write(true).open(p).map(drop));
        #[cfg(not(windows))]
        {
            let _ = game_dir;
            None
        }
    }
}

/// The first `.exe` directly in `game_dir` that `open_for_write` refuses: a
/// running program's file cannot be opened for writing on Windows, so this
/// catches a client whose name is on no list. Anything unclear (a folder that
/// cannot be listed, an exe that cannot be opened for another reason) counts
/// as in use. An exe deleted meanwhile does not.
#[cfg_attr(not(windows), allow(dead_code))]
fn held_exe(game_dir: &Path, open_for_write: impl Fn(&Path) -> io::Result<()>) -> Option<String> {
    let entries = match std::fs::read_dir(game_dir) {
        Ok(e) => e,
        Err(e) => return Some(format!("{} cannot be listed: {e}", game_dir.display())),
    };
    for entry in entries {
        let path = match entry {
            Ok(e) => e.path(),
            Err(e) => return Some(format!("{} cannot be listed: {e}", game_dir.display())),
        };
        if !path.extension().is_some_and(|x| x.eq_ignore_ascii_case("exe")) || !path.is_file() {
            continue;
        }
        match open_for_write(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => {
                let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                return Some(format!("{name} is in use: {e}"));
            }
        }
    }
    None
}

/// True when `text` (lowercase) names a Windows client as a whole file name:
/// after the start, a path separator, a space or a quote, and before the end,
/// a space or a quote. `WowUp.exe` or `wow.exe.bak` do not count.
fn names_a_windows_client(text: &str) -> bool {
    let edge = |c: Option<char>| c.map_or(true, |c| c.is_whitespace() || matches!(c, '"' | '\''));
    WOW_PROCESS_NAMES.iter().filter(|n| n.ends_with(".exe")).any(|name| {
        text.match_indices(name).any(|(i, _)| {
            let before = text[..i].chars().next_back();
            (edge(before) || matches!(before, Some('/' | '\\'))) && edge(text[i + name.len()..].chars().next())
        })
    })
}

/// First CSV column of each `tasklist /FO CSV /NH` line.
#[cfg_attr(not(windows), allow(dead_code))]
fn parse_tasklist(out: &str) -> Vec<String> {
    out.lines()
        .filter_map(|line| {
            let rest = line.trim().strip_prefix('"')?;
            Some(rest.split('"').next()?.to_string())
        })
        .collect()
}

/// True when any WoW client runs. A listing that cannot be read also counts
/// as running: nothing is swapped when it is unknown whether the game is open.
pub fn wow_is_running(p: &dyn GameProcesses) -> bool {
    let Ok(list) = p.executables() else { return true };
    // A process list with nothing in it is a failed read, not a quiet machine.
    if list.is_empty() {
        return true;
    }
    list.iter().any(|entry| {
        let lower = entry.trim().to_ascii_lowercase();
        let base = lower.rsplit(['/', '\\']).next().unwrap_or(&lower).trim();
        WOW_PROCESS_NAMES.contains(&base)
            || (base.starts_with("world of warcraft") && !base.contains("launcher"))
            || names_a_windows_client(&lower)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    struct Fake(Result<Vec<&'static str>, ()>);

    impl GameProcesses for Fake {
        fn executables(&self) -> std::io::Result<Vec<String>> {
            match &self.0 {
                Ok(v) => Ok(v.iter().map(|s| s.to_string()).collect()),
                Err(()) => Err(std::io::Error::other("no listing")),
            }
        }
    }

    fn running(names: &[&'static str]) -> bool {
        wow_is_running(&Fake(Ok(names.to_vec())))
    }

    #[test]
    fn no_wow_means_not_running() {
        assert!(!running(&["explorer.exe", "Battle.net.exe"]));
    }

    #[test]
    fn retail_windows_exe() {
        assert!(running(&["explorer.exe", "Wow.exe"]));
    }

    #[test]
    fn forever_beta_exe_any_case() {
        assert!(running(&["WOWCLASSICB.EXE"]));
    }

    #[test]
    fn macos_full_path() {
        assert!(running(&[
            "/Applications/World of Warcraft/_retail_/World of Warcraft.app/Contents/MacOS/World of Warcraft"
        ]));
    }

    #[test]
    fn windows_path_with_backslashes() {
        assert!(running(&["C:\\Program Files (x86)\\World of Warcraft\\_retail_\\Wow.exe"]));
    }

    #[test]
    fn the_launcher_is_not_the_game() {
        assert!(!running(&[
            "/Applications/World of Warcraft/World of Warcraft Launcher",
            "Battle.net Launcher.exe"
        ]));
    }

    #[test]
    fn a_listing_that_fails_counts_as_running() {
        assert!(wow_is_running(&Fake(Err(()))));
    }

    #[test]
    fn any_macos_binary_named_world_of_warcraft_counts() {
        assert!(running(&["/Applications/World of Warcraft/_classic_beta_/World of Warcraft Classic Beta.app/Contents/MacOS/World of Warcraft Classic Beta"]));
        assert!(running(&["World of Warcraft Test"]));
        assert!(running(&["world of warcraft classic ptr"]));
    }

    #[test]
    fn launchers_with_the_game_name_are_not_the_game() {
        assert!(!running(&["World of Warcraft Launcher", "world of warcraft classic launcher"]));
    }

    #[test]
    fn windows_arm64_and_test_clients() {
        for n in ["Wow-ARM64.exe", "WowB-ARM64.exe", "WowT-ARM64.exe", "WowClassic-ARM64.exe", "WowClassicB-ARM64.exe", "WowClassicT-ARM64.exe", "WowB.exe", "WowT.exe", "WowClassicT.exe"] {
            assert!(running(&[n]), "{n}");
        }
    }

    #[test]
    fn an_empty_listing_counts_as_running() {
        assert!(running(&[]));
    }

    #[test]
    fn a_wine_client_listed_by_its_command_line_counts() {
        // Some Linux launchers leave `comm` as Wine's preloader; the command line still names the exe.
        assert!(running(&["wine64-preloade C:\\Program Files (x86)\\World of Warcraft\\_retail_\\Wow.exe -launcherlogin"]));
        assert!(running(&["/usr/bin/wine64-preloader Z:\\home\\me\\Games\\World of Warcraft\\_classic_beta_\\WowClassicB.exe"]));
        assert!(running(&["\"C:\\World of Warcraft\\_retail_\\Wow.exe\" -launcherlogin"]));
        assert!(running(&["wine Wow.exe"]));
    }

    #[test]
    fn programs_that_only_mention_a_client_name_are_not_the_game() {
        assert!(!running(&[
            "WowUp.exe",
            "wine64-preloade C:\\Program Files (x86)\\WowUp\\WowUp.exe",
            "wine64-preloade C:\\Program Files (x86)\\Battle.net\\Battle.net.exe --exec=\"launch WoW\"",
            "/usr/bin/vim /home/me/wow.exe.bak",
            "/usr/bin/cp /tmp/notwow.exe /tmp/b",
        ]));
    }

    fn game_dir(files: &[&str]) -> tempfile::TempDir {
        let t = tempfile::tempdir().unwrap();
        for f in files {
            let p = t.path().join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, "x").unwrap();
        }
        t
    }

    fn held_by_name(held: &'static [&'static str]) -> impl Fn(&Path) -> io::Result<()> {
        move |p: &Path| {
            let name = p.file_name().unwrap().to_string_lossy().to_string();
            if held.contains(&name.as_str()) {
                Err(io::Error::other("being used by another process"))
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn an_exe_in_the_game_folder_that_cannot_be_opened_for_writing_is_the_game() {
        let d = game_dir(&["Wow.exe", "WowVoiceProxy.exe", "Cache/x.txt"]);
        assert_eq!(held_exe(d.path(), held_by_name(&[])), None);
        assert!(held_exe(d.path(), held_by_name(&["Wow.exe"])).unwrap().contains("Wow.exe"));
        // Any name: a client the name list does not know is caught too.
        let d = game_dir(&["WowClassicSomething.EXE"]);
        assert!(held_exe(d.path(), held_by_name(&["WowClassicSomething.EXE"])).is_some());
    }

    #[test]
    fn only_exe_files_directly_in_the_game_folder_count() {
        let d = game_dir(&["Utils/WowVoiceProxy.exe", "Wow.dll", "notes.txt"]);
        assert_eq!(held_exe(d.path(), held_by_name(&["WowVoiceProxy.exe", "Wow.dll", "notes.txt"])), None);
    }

    #[test]
    fn an_exe_that_vanished_is_not_the_game_but_an_unreadable_folder_is() {
        let d = game_dir(&["Wow.exe"]);
        let gone = |_: &Path| Err(io::Error::from(io::ErrorKind::NotFound));
        assert_eq!(held_exe(d.path(), gone), None);
        assert!(held_exe(&d.path().join("missing"), held_by_name(&[])).is_some());
    }

    #[cfg(windows)]
    #[test]
    fn a_running_exe_cannot_be_opened_for_writing() {
        // This test's own binary runs from its folder, so that folder holds a running exe.
        let exe = std::env::current_exe().unwrap();
        assert!(SystemProcesses.exe_in_use(exe.parent().unwrap()).is_some());
        assert_eq!(SystemProcesses.exe_in_use(game_dir(&["Wow.exe"]).path()), None);
    }

    #[test]
    fn parses_tasklist_csv() {
        let out = "\"Wow.exe\",\"1234\",\"Console\",\"1\",\"900,000 K\"\r\n\"System\",\"4\",\"Services\",\"0\",\"12 K\"\r\n";
        assert_eq!(parse_tasklist(out), vec!["Wow.exe".to_string(), "System".to_string()]);
    }
}
