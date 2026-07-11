//! Minimal, dependency-free file logging: append-only with a naive size cap
//! (truncate — not rotate — once the file crosses 1 MiB). A companion this
//! small doesn't need a logging framework; a plain mutex-guarded appender
//! is easier to reason about and to unit test.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub const LOG_FILE_NAME: &str = "companion.log";

/// Naive cap: once the log file is larger than this, it is truncated to
/// empty before the next line is appended. No rotation, no archiving — this
/// is a debugging aid for a tray app, not an audit trail.
const MAX_LOG_BYTES: u64 = 1024 * 1024;

pub struct Logger {
    path: PathBuf,
    write_lock: Mutex<()>,
}

impl Logger {
    pub fn new(dir: &Path) -> std::io::Result<Logger> {
        fs::create_dir_all(dir)?;
        Ok(Logger {
            path: dir.join(LOG_FILE_NAME),
            write_lock: Mutex::new(()),
        })
    }

    pub fn info(&self, message: &str) {
        self.log("INFO", message);
    }

    pub fn error(&self, message: &str) {
        self.log("ERROR", message);
    }

    fn log(&self, level: &str, message: &str) {
        let _guard = self.write_lock.lock().unwrap_or_else(|p| p.into_inner());
        if let Err(e) = self.append_line(level, message) {
            // Logging must never take down the sync loop — the best we can
            // do on a write failure is tell stderr and move on.
            eprintln!("companion: failed to write {}: {e}", self.path.display());
        }
    }

    fn append_line(&self, level: &str, message: &str) -> std::io::Result<()> {
        cap_file_size(&self.path, MAX_LOG_BYTES)?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        writeln!(
            file,
            "[{}] {level:<5} {message}",
            format_unix_utc(now_unix())
        )
    }
}

fn cap_file_size(path: &Path, max_bytes: u64) -> std::io::Result<()> {
    match fs::metadata(path) {
        Ok(meta) if meta.len() > max_bytes => fs::write(path, b""),
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

fn now_unix() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Renders a unix timestamp as `YYYY-MM-DD HH:MM:SS` UTC without pulling in
/// a date/time crate. Uses Howard Hinnant's `civil_from_days` algorithm
/// (public domain, http://howardhinnant.github.io/date_algorithms.html).
pub fn format_unix_utc(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let secs_of_day = secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = secs_of_day / 3600;
    let minute = (secs_of_day % 3600) / 60;
    let second = secs_of_day % 60;
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}")
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    let year = if m <= 2 { y + 1 } else { y };
    (year, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn formats_epoch_zero() {
        assert_eq!(format_unix_utc(0), "1970-01-01 00:00:00");
    }

    #[test]
    fn formats_known_timestamps() {
        assert_eq!(format_unix_utc(1_700_000_000), "2023-11-14 22:13:20");
        assert_eq!(format_unix_utc(1_609_459_200), "2021-01-01 00:00:00");
    }

    #[test]
    fn logger_creates_dir_and_appends_lines() {
        let dir =
            std::env::temp_dir().join(format!("goldcap-companion-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let logger = Logger::new(&dir).expect("logger dir should be creatable");
        logger.info("hello");
        logger.error("boom");

        let mut contents = String::new();
        fs::File::open(dir.join(LOG_FILE_NAME))
            .unwrap()
            .read_to_string(&mut contents)
            .unwrap();
        assert!(contents.contains("INFO"));
        assert!(contents.contains("hello"));
        assert!(contents.contains("ERROR"));
        assert!(contents.contains("boom"));

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn caps_log_file_at_naive_size_limit() {
        let dir =
            std::env::temp_dir().join(format!("goldcap-companion-test-cap-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(LOG_FILE_NAME);
        // Pre-fill past the cap.
        fs::write(&path, vec![b'x'; (MAX_LOG_BYTES + 1) as usize]).unwrap();

        let logger = Logger::new(&dir).unwrap();
        logger.info("after cap");

        let mut contents = String::new();
        fs::File::open(&path)
            .unwrap()
            .read_to_string(&mut contents)
            .unwrap();
        // Truncated, not rotated: no leftover 'x' filler, just the new line.
        assert!(!contents.contains('x'));
        assert!(contents.contains("after cap"));

        fs::remove_dir_all(&dir).ok();
    }
}
