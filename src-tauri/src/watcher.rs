//! Triggers a sync the moment WoW writes SavedVariables to disk, so ledger
//! rows reach the site seconds after a logout or `/reload` instead of
//! waiting out the interval poll. The interval loop stays as the fallback —
//! if the filesystem watcher dies or the platform never delivers events,
//! behavior degrades to exactly what it was before this module existed.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::{mpsc, watch};

use crate::config::Config;
use crate::logging::Logger;

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

/// Watches `<wow_retail_path>/WTF/Account` and fires `trigger_tx` (the same
/// channel as the tray's "Sync now") after a debounced SavedVariables write.
/// Rebuilds the watcher whenever the config (and thus possibly the WoW path)
/// changes. Lives for the whole app run.
pub async fn run_loop(
    mut config_rx: watch::Receiver<Config>,
    trigger_tx: mpsc::Sender<()>,
    logger: Arc<Logger>,
) {
    loop {
        let wow_path = config_rx.borrow().wow_retail_path.trim().to_string();
        let (event_tx, mut event_rx) = mpsc::unbounded_channel::<()>();

        // Held for its Drop: dropping the watcher (on config change) stops
        // the notify thread watching the old path.
        let _watcher = if wow_path.is_empty() {
            None
        } else {
            build_watcher(
                PathBuf::from(&wow_path).join("WTF").join("Account"),
                event_tx,
                &logger,
            )
        };

        let mut debounce = Debouncer::default();
        loop {
            // A pending deadline bounds the select; otherwise wait forever.
            let sleep_until = debounce
                .fire_at()
                .unwrap_or_else(|| Instant::now() + Duration::from_secs(3600));
            tokio::select! {
                changed = config_rx.changed() => {
                    if changed.is_err() {
                        return; // config sender dropped — app shutting down
                    }
                    break; // rebuild the watcher against the (new) path
                }
                received = event_rx.recv() => {
                    match received {
                        Some(()) => debounce.on_event(Instant::now()),
                        None => {
                            // notify thread died; interval remains as
                            // fallback. Park until the config changes.
                            let _ = config_rx.changed().await;
                            break;
                        }
                    }
                }
                _ = tokio::time::sleep_until(sleep_until.into()) => {
                    if debounce.on_deadline(Instant::now()) {
                        logger.info("savedvars changed, syncing");
                        let _ = trigger_tx.try_send(());
                    }
                }
            }
        }
    }
}

/// One recursive watcher on the account root. Filtering happens here, on
/// notify's own thread, so the async side only ever sees relevant events.
fn build_watcher(
    account_root: PathBuf,
    event_tx: mpsc::UnboundedSender<()>,
    logger: &Logger,
) -> Option<RecommendedWatcher> {
    let watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(event) = res {
            if event.paths.iter().any(|p| is_goldcap_savedvars(p)) {
                let _ = event_tx.send(());
            }
        }
    });
    let mut watcher = match watcher {
        Ok(w) => w,
        Err(e) => {
            logger.error(&format!("savedvars watcher failed to start: {e}"));
            return None;
        }
    };
    if let Err(e) = watcher.watch(&account_root, RecursiveMode::Recursive) {
        // Normal on a fresh install with no WoW path yet, or a moved dir;
        // the interval fallback covers it and a config change retries.
        logger.error(&format!(
            "savedvars watcher cannot watch {}: {e}",
            account_root.display()
        ));
        return None;
    }
    logger.info(&format!(
        "watching {} for savedvars writes",
        account_root.display()
    ));
    Some(watcher)
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

    #[tokio::test]
    async fn watcher_reports_a_goldcap_write() {
        let dir = std::env::temp_dir().join(format!(
            "goldcap-watcher-test-{}",
            std::process::id()
        ));
        let sv = dir.join("Account").join("ACC1").join("SavedVariables");
        std::fs::create_dir_all(&sv).unwrap();

        let logger_dir = dir.join("log");
        std::fs::create_dir_all(&logger_dir).unwrap();
        let logger = crate::logging::Logger::new(&logger_dir).unwrap();

        let (tx, mut rx) = mpsc::unbounded_channel();
        let _w = build_watcher(dir.join("Account"), tx, &logger)
            .expect("watcher should start on an existing dir");

        std::fs::write(sv.join("GoldCap.lua"), "GOLDCAP_DB = {}").unwrap();

        let got = tokio::time::timeout(Duration::from_secs(10), rx.recv()).await;
        assert!(got.is_ok(), "no event within 10s for a GoldCap.lua write");

        std::fs::remove_dir_all(&dir).ok();
    }
}
