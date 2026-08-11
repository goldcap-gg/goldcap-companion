//! Triggers a sync the moment WoW writes SavedVariables to disk, so ledger
//! rows reach the site seconds after a logout or `/reload` instead of
//! waiting out the interval poll. The interval loop stays as the fallback —
//! if the filesystem watcher dies or the platform never delivers events,
//! behavior degrades to exactly what it was before this module existed.

use std::path::Path;
use std::time::{Duration, Instant};

/// Quiet period after the last matching event before firing one sync. WoW
/// rewrites SavedVariables non-atomically (and per account), so syncing on
/// the first event would read a half-written file. Harmless — the Lua parse
/// fails and the interval retries — but pointless.
pub const DEBOUNCE: Duration = Duration::from_secs(3);

/// True for any path shaped `…/SavedVariables/GoldCap.lua`, the file
/// `savedvars::saved_variables_paths` enumerates per account.
pub fn is_goldcap_savedvars(path: &Path) -> bool {
    path.file_name().is_some_and(|f| f == "GoldCap.lua")
        && path
            .parent()
            .and_then(|p| p.file_name())
            .is_some_and(|d| d == "SavedVariables")
}

/// Debounce state: every event pushes the deadline out; the deadline firing
/// resets the state. Pure so it can be tested without a runtime.
#[derive(Default)]
pub struct Debouncer {
    deadline: Option<Instant>,
}

impl Debouncer {
    pub fn on_event(&mut self, now: Instant) {
        self.deadline = Some(now + DEBOUNCE);
    }

    pub fn fire_at(&self) -> Option<Instant> {
        self.deadline
    }

    /// Returns true when the deadline has passed and a sync should fire;
    /// clears the state either way only when it fires.
    pub fn on_deadline(&mut self, now: Instant) -> bool {
        match self.deadline {
            Some(d) if now >= d => {
                self.deadline = None;
                true
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn matches_the_savedvars_file() {
        let p = PathBuf::from("/wow/WTF/Account/ACC1/SavedVariables/GoldCap.lua");
        assert!(is_goldcap_savedvars(&p));
    }

    #[test]
    fn rejects_other_addons_and_other_dirs() {
        assert!(!is_goldcap_savedvars(Path::new(
            "/wow/WTF/Account/ACC1/SavedVariables/TSM.lua"
        )));
        assert!(!is_goldcap_savedvars(Path::new(
            "/wow/WTF/Account/ACC1/GoldCap.lua"
        )));
        assert!(!is_goldcap_savedvars(Path::new(
            "/wow/WTF/Account/ACC1/SavedVariables/GoldCap.lua.bak"
        )));
    }

    #[test]
    fn debouncer_waits_out_a_burst_then_fires_once() {
        let mut d = Debouncer::default();
        let t0 = Instant::now();
        d.on_event(t0);
        // A second event 1 s in pushes the deadline out.
        d.on_event(t0 + Duration::from_secs(1));
        assert_eq!(d.fire_at(), Some(t0 + Duration::from_secs(1) + DEBOUNCE));
        // Deadline not reached yet — must not fire.
        assert!(!d.on_deadline(t0 + Duration::from_secs(2)));
        // Reached — fires exactly once, then goes quiet.
        assert!(d.on_deadline(t0 + Duration::from_secs(4) + DEBOUNCE));
        assert!(!d.on_deadline(t0 + Duration::from_secs(9)));
        assert_eq!(d.fire_at(), None);
    }
}
