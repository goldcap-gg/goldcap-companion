//! Whether World of Warcraft is running, behind a trait so the install logic
//! can be tested with a fake process list. No process-listing crate: std, and
//! on Windows the system calls from `windows-sys`. A wrong answer has to err
//! towards "running": an update applied under a running game can leave it half
//! old, half new.

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

    /// Full paths of the running programs' files. Only Windows is asked;
    /// elsewhere this is empty and the name list is the whole check.
    fn image_paths(&self) -> io::Result<Vec<String>> {
        Ok(Vec::new())
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

    /// Every process the user may query, by the least access Windows has
    /// (`PROCESS_QUERY_LIMITED_INFORMATION`, what Task Manager uses for the same
    /// column); a process that cannot be opened is skipped. Nothing is read
    /// from a process but its file's path.
    #[cfg(windows)]
    fn image_paths(&self) -> io::Result<Vec<String>> {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::ProcessStatus::K32EnumProcesses;
        use windows_sys::Win32::System::Threading::{
            OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
        };

        let mut pids = vec![0u32; 1024];
        loop {
            let size = std::mem::size_of_val(pids.as_slice()) as u32;
            let mut used = 0u32;
            // SAFETY: `pids` holds `size` bytes, and both pointers outlive the call.
            if unsafe { K32EnumProcesses(pids.as_mut_ptr(), size, &mut used) } == 0 {
                return Err(io::Error::last_os_error());
            }
            if used < size {
                pids.truncate(used as usize / std::mem::size_of::<u32>());
                break;
            }
            // A full buffer may have cut the list short: ask again with a bigger one.
            if pids.len() >= 1 << 20 {
                return Err(io::Error::other("the process list does not fit"));
            }
            pids.resize(pids.len() * 2, 0);
        }
        let mut buf = vec![0u16; 32_768];
        let mut images = Vec::new();
        for pid in pids {
            // SAFETY: a plain call; the handle is checked before use.
            let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
            if handle.is_null() {
                continue;
            }
            let mut len = buf.len() as u32;
            // SAFETY: `buf` holds `len` UTF-16 units and `handle` stays open until CloseHandle.
            let ok = unsafe { QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len) };
            // SAFETY: `handle` came from OpenProcess above and is closed once.
            unsafe { CloseHandle(handle) };
            if ok != 0 {
                images.push(String::from_utf16_lossy(&buf[..len as usize]));
            }
        }
        // This program can always read its own path: none at all is a failed read.
        if images.is_empty() {
            return Err(io::Error::other("no running program's path could be read"));
        }
        Ok(images)
    }
}

/// Why a program runs directly from `game_dir`, `None` when none does, or why
/// that cannot be told (every caller then treats the game as running). This
/// catches a client whose name is on no list, and never opens a game file.
pub fn runs_from(p: &dyn GameProcesses, game_dir: &Path) -> Result<Option<String>, String> {
    let images = p
        .image_paths()
        .map_err(|e| format!("the folders of the running programs cannot be read: {e}"))?;
    // A folder reached through a link shows up under its real path.
    let real = game_dir.canonicalize().ok();
    Ok(std::iter::once(game_dir)
        .chain(real.as_deref())
        .find_map(|dir| image_in_folder(&images, dir))
        .map(|name| format!("{name} runs from the game folder")))
}

/// The file name of the first of `images` that sits directly in `game_dir`:
/// not in a folder under it or beside it.
fn image_in_folder(images: &[String], game_dir: &Path) -> Option<String> {
    let dir = windows_path_parts(&game_dir.to_string_lossy());
    if dir.is_empty() {
        return None;
    }
    images.iter().find_map(|image| {
        let mut parts = windows_path_parts(image);
        parts.pop()?;
        (parts == dir).then(|| image.rsplit(['/', '\\']).next().unwrap_or(image).to_string())
    })
}

/// `path` as Windows compares it: in any case, with either slash, without a
/// `\\?\` (or `\\?\UNC\`) prefix, with `.` and `..` worked out and no
/// empty parts.
fn windows_path_parts(path: &str) -> Vec<String> {
    let lower = path.to_lowercase().replace('/', "\\");
    let rest = lower
        .strip_prefix(r"\\?\unc\")
        .or_else(|| lower.strip_prefix(r"\\?\"))
        .unwrap_or(&lower);
    let mut parts: Vec<String> = Vec::new();
    for part in rest.split('\\') {
        match part {
            "" | "." => {}
            // Never above the drive: `C:\..` is still `C:\`.
            ".." => {
                if parts.last().is_some_and(|p| !p.ends_with(':')) {
                    parts.pop();
                }
            }
            _ => parts.push(part.to_string()),
        }
    }
    parts
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

/// Whether any WoW client runs, or why that cannot be told. Every caller
/// treats `Err` as running: nothing is swapped when it is unknown whether the
/// game is open, and the reason goes to the card instead of blaming the game.
pub fn wow_is_running(p: &dyn GameProcesses) -> Result<bool, String> {
    let list = p
        .executables()
        .map_err(|e| format!("the list of running programs cannot be read: {e}"))?;
    // A process list with nothing in it is a failed read, not a quiet machine.
    if list.is_empty() {
        return Err("the list of running programs came back empty".to_string());
    }
    Ok(list.iter().any(|entry| {
        let lower = entry.trim().to_ascii_lowercase();
        let base = lower.rsplit(['/', '\\']).next().unwrap_or(&lower).trim();
        WOW_PROCESS_NAMES.contains(&base)
            || (base.starts_with("world of warcraft") && !base.contains("launcher"))
            || names_a_windows_client(&lower)
    }))
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
        wow_is_running(&Fake(Ok(names.to_vec()))).unwrap()
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
    fn a_listing_that_fails_cannot_tell_and_says_why() {
        assert!(wow_is_running(&Fake(Err(()))).unwrap_err().contains("no listing"));
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
    fn an_empty_listing_cannot_tell() {
        assert!(wow_is_running(&Fake(Ok(vec![]))).is_err());
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

    /// Lists `explorer.exe` by name, and gives out these program paths.
    struct Images(Result<Vec<&'static str>, ()>);

    impl GameProcesses for Images {
        fn executables(&self) -> std::io::Result<Vec<String>> {
            Ok(vec!["explorer.exe".to_string()])
        }

        fn image_paths(&self) -> std::io::Result<Vec<String>> {
            match &self.0 {
                Ok(v) => Ok(v.iter().map(|s| s.to_string()).collect()),
                Err(()) => Err(std::io::Error::other("access is denied")),
            }
        }
    }

    fn in_folder(images: &[&str], game_dir: &str) -> Option<String> {
        let images: Vec<String> = images.iter().map(|s| s.to_string()).collect();
        image_in_folder(&images, Path::new(game_dir))
    }

    const RETAIL: &str = r"C:\Program Files (x86)\World of Warcraft\_retail_";

    #[test]
    fn a_program_directly_in_the_game_folder_is_the_game_whatever_its_name() {
        let images = [r"C:\Windows\explorer.exe", r"C:\Program Files (x86)\World of Warcraft\_retail_\WowNewClient.exe"];
        assert_eq!(in_folder(&images, RETAIL).as_deref(), Some("WowNewClient.exe"));
        assert_eq!(in_folder(&[r"C:\Windows\explorer.exe"], RETAIL), None);
        assert_eq!(in_folder(&[], RETAIL), None);
    }

    #[test]
    fn a_program_in_a_subfolder_or_a_sibling_folder_is_not_the_game() {
        for image in [
            r"C:\Program Files (x86)\World of Warcraft\_retail_\Utils\WowVoiceProxy.exe",
            r"C:\Program Files (x86)\World of Warcraft\_retail_\..\Battle.net\Battle.net.exe",
            r"C:\Program Files (x86)\World of Warcraft\World of Warcraft Launcher.exe",
            r"C:\Program Files (x86)\World of Warcraft\_retail_2\Wow.exe",
            r"C:\Program Files (x86)\World of Warcraft\_retail_.exe",
            r"D:\Program Files (x86)\World of Warcraft\_retail_\Wow.exe",
        ] {
            assert_eq!(in_folder(&[image], RETAIL), None, "{image}");
        }
    }

    #[test]
    fn case_slashes_dots_and_a_trailing_separator_do_not_matter() {
        assert!(in_folder(&["c:/program files (x86)/WORLD OF WARCRAFT/_Retail_/wow.exe"], RETAIL).is_some());
        assert!(in_folder(&[r"C:\Program Files (x86)\World of Warcraft\_retail_\Wow.exe"], r"C:\Program Files (x86)\World of Warcraft\_retail_\").is_some());
        assert!(in_folder(&[r"C:\Program Files (x86)\World of Warcraft\.\_retail_\Wow.exe"], r"C:/Program Files (x86)//World of Warcraft/_classic_/../_retail_").is_some());
        assert!(in_folder(&[r"C:\Игры\World of Warcraft\_retail_\Wow.exe"], r"c:\игры\world of warcraft\_retail_").is_some());
        assert!(in_folder(&[r"C:\..\Program Files (x86)\World of Warcraft\_retail_\Wow.exe"], RETAIL).is_some());
    }

    #[test]
    fn a_long_path_prefix_on_either_side_is_the_same_folder() {
        assert!(in_folder(&[r"\\?\C:\Program Files (x86)\World of Warcraft\_retail_\Wow.exe"], RETAIL).is_some());
        assert!(in_folder(&[r"C:\Program Files (x86)\World of Warcraft\_retail_\Wow.exe"], r"\\?\C:\Program Files (x86)\World of Warcraft\_retail_").is_some());
        assert!(in_folder(&[r"\\?\UNC\nas\games\World of Warcraft\_retail_\Wow.exe"], r"\\nas\games\World of Warcraft\_retail_").is_some());
        assert!(in_folder(&[r"\\nas\games\World of Warcraft\_retail_\Wow.exe"], r"\\?\unc\NAS\games\World of Warcraft\_retail_").is_some());
        assert_eq!(in_folder(&[r"\\?\UNC\nas\games\World of Warcraft\_retail_\Wow.exe"], r"C:\nas\games\World of Warcraft\_retail_"), None);
    }

    #[test]
    fn an_empty_game_folder_matches_nothing() {
        assert_eq!(in_folder(&["Wow.exe", r"\Wow.exe"], ""), None);
    }

    #[test]
    fn runs_from_names_the_program_or_says_why_it_cannot_tell() {
        let found = runs_from(&Images(Ok(vec![r"C:\Program Files (x86)\World of Warcraft\_retail_\WowNewClient.exe"])), Path::new(RETAIL));
        assert!(found.unwrap().unwrap().contains("WowNewClient.exe"));
        assert_eq!(runs_from(&Images(Ok(vec![r"C:\Windows\explorer.exe"])), Path::new(RETAIL)), Ok(None));
        // A system that gives out no paths at all leaves the name list as the whole check.
        assert_eq!(runs_from(&Images(Ok(vec![])), Path::new(RETAIL)), Ok(None));
        assert!(runs_from(&Images(Err(())), Path::new(RETAIL)).unwrap_err().contains("access is denied"));
    }

    #[cfg(windows)]
    #[test]
    fn this_test_runs_from_its_own_folder() {
        let exe = std::env::current_exe().unwrap();
        let found = runs_from(&SystemProcesses, exe.parent().unwrap()).unwrap();
        assert!(found.is_some());
        let other = tempfile::tempdir().unwrap();
        assert_eq!(runs_from(&SystemProcesses, other.path()), Ok(None));
    }

    #[test]
    fn parses_tasklist_csv() {
        let out = "\"Wow.exe\",\"1234\",\"Console\",\"1\",\"900,000 K\"\r\n\"System\",\"4\",\"Services\",\"0\",\"12 K\"\r\n";
        assert_eq!(parse_tasklist(out), vec!["Wow.exe".to_string(), "System".to_string()]);
    }
}
