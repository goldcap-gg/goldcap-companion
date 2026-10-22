//! Whether World of Warcraft is running, behind a trait so the install logic
//! can be tested with a fake process list. Only std is used: no
//! process-listing crate.

use std::io;
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
    /// Process executable names or paths, one per process.
    fn executables(&self) -> io::Result<Vec<String>>;
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
        #[cfg(not(windows))]
        let output = Command::new("ps").args(["-A", "-o", "comm="]).output()?;
        if !output.status.success() {
            return Err(io::Error::other(format!("process listing exited with {}", output.status)));
        }
        let text = String::from_utf8_lossy(&output.stdout);
        #[cfg(windows)]
        return Ok(parse_tasklist(&text));
        #[cfg(not(windows))]
        return Ok(text.lines().map(str::to_string).collect());
    }
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
        let base = entry.rsplit(['/', '\\']).next().unwrap_or(entry).trim().to_ascii_lowercase();
        WOW_PROCESS_NAMES.contains(&base.as_str())
            || (base.starts_with("world of warcraft") && !base.contains("launcher"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn parses_tasklist_csv() {
        let out = "\"Wow.exe\",\"1234\",\"Console\",\"1\",\"900,000 K\"\r\n\"System\",\"4\",\"Services\",\"0\",\"12 K\"\r\n";
        assert_eq!(parse_tasklist(out), vec!["Wow.exe".to_string(), "System".to_string()]);
    }
}
