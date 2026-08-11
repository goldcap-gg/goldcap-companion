//! Auto-update: silent periodic check + download, tray-driven restart.
//!
//! Platform split (spec 2026-08-11): macOS installs immediately (the
//! extracted .app applies on next launch) and the tray offers a restart;
//! Windows can't install without NSIS killing the app, so the downloaded
//! installer is staged in memory and only runs when the user clicks the
//! tray item.

use crate::logging::Logger;
use crate::tray;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::AppHandle;
use tauri_plugin_updater::UpdaterExt;

/// Don't compete with the first addon sync right after startup.
pub const CHECK_STARTUP_DELAY: Duration = Duration::from_secs(60);
pub const CHECK_INTERVAL: Duration = Duration::from_secs(4 * 60 * 60);

/// How far a found update has progressed on this platform.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StagedKind {
    /// Already written to disk; a restart applies it (macOS).
    Installed,
    /// Downloaded, waiting for the user to allow the installer run (Windows).
    Pending,
}

/// Tray menu label for a staged update.
pub fn tray_label(kind: StagedKind, version: &str) -> String {
    match kind {
        StagedKind::Installed => format!("Restart to update to v{version}"),
        StagedKind::Pending => format!("Install update v{version} and restart"),
    }
}

/// A found update, staged as far as this platform allows.
pub enum Staged {
    Installed { version: String },
    // Kept in memory (NSIS installer, a few MB) so the click handler can
    // run it without a re-download; dropped if the app quits first, and
    // the next startup check simply re-finds the same version.
    #[cfg_attr(not(windows), allow(dead_code))]
    Pending {
        version: String,
        update: tauri_plugin_updater::Update,
        bytes: Vec<u8>,
    },
}

impl Staged {
    pub fn kind(&self) -> StagedKind {
        match self {
            Staged::Installed { .. } => StagedKind::Installed,
            Staged::Pending { .. } => StagedKind::Pending,
        }
    }
    pub fn version(&self) -> &str {
        match self {
            Staged::Installed { version } | Staged::Pending { version, .. } => version,
        }
    }
}

#[derive(Default)]
pub struct UpdaterState {
    pub staged: Mutex<Option<Staged>>,
}

/// Startup-delayed periodic check. Never dialogs; failures log and the
/// next tick retries from scratch.
pub async fn run_loop(app: AppHandle, logger: Arc<Logger>) {
    tokio::time::sleep(CHECK_STARTUP_DELAY).await;
    loop {
        if let Err(e) = check_once(&app, &logger).await {
            logger.error(&format!("update check failed: {e}"));
        }
        tokio::time::sleep(CHECK_INTERVAL).await;
    }
}

async fn check_once(app: &AppHandle, logger: &Logger) -> tauri_plugin_updater::Result<()> {
    use tauri::Manager;
    let state = app.state::<UpdaterState>();

    // Something is already staged: don't download again, just keep the
    // tray offer visible. The guard must be dropped before touching the
    // menu — show_update_item blocks on the main thread, and the main
    // thread's menu handler (apply_staged) takes this same mutex, so
    // calling it while still holding the guard is a lock-order inversion
    // that can deadlock (same pattern as tray::refresh).
    let staged_label = {
        let staged = state.staged.lock().unwrap_or_else(|p| p.into_inner());
        staged.as_ref().map(|s| tray_label(s.kind(), s.version()))
    };
    if let Some(label) = staged_label {
        tray::show_update_item(app, &label);
        return Ok(());
    }

    let Some(update) = app.updater()?.check().await? else {
        return Ok(());
    };
    let version = update.version.clone();
    logger.info(&format!("update available: v{version}, downloading"));
    let bytes = update.download(|_, _| {}, || {}).await?;

    #[cfg(not(windows))]
    let staged = {
        // macOS: extract over the .app now; takes effect on relaunch. Log
        // an install failure distinctly from a check failure (the `?` in
        // `check()` above vs. here) so the two are diagnosable apart; this
        // is log-only, and returning Ok(()) lets the next tick retry.
        if let Err(e) = update.install(bytes) {
            logger.error(&format!("update install failed: {e}"));
            return Ok(());
        }
        logger.info(&format!("update v{version} installed, restart to apply"));
        Staged::Installed { version }
    };
    #[cfg(windows)]
    let staged = {
        logger.info(&format!("update v{version} downloaded, waiting for restart"));
        Staged::Pending { version, update, bytes }
    };

    let label = tray_label(staged.kind(), staged.version());
    *state.staged.lock().unwrap_or_else(|p| p.into_inner()) = Some(staged);
    tray::show_update_item(app, &label);
    Ok(())
}

/// Tray click: apply whatever is staged. Takes the staged value out so a
/// failed Windows install can be retried by the next periodic check.
pub fn apply_staged(app: &AppHandle) {
    use tauri::Manager;
    let state = app.state::<UpdaterState>();
    let staged = state
        .staged
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .take();
    match staged {
        None => {}
        Some(Staged::Installed { .. }) => app.restart(),
        Some(Staged::Pending { version, update, bytes }) => {
            // On success the installer exits this process, so this arm only
            // continues on failure — put the update back so the tray item
            // keeps working instead of becoming a silent no-op.
            if let Err(e) = update.install(&bytes) {
                let logger = app.state::<crate::state::AppState>().logger.clone();
                logger.error(&format!("update install failed: {e}"));
                *state.staged.lock().unwrap_or_else(|p| p.into_inner()) =
                    Some(Staged::Pending { version, update, bytes });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installed_label_offers_restart() {
        assert_eq!(
            tray_label(StagedKind::Installed, "1.3.0"),
            "Restart to update to v1.3.0"
        );
    }

    #[test]
    fn pending_label_offers_install_and_restart() {
        assert_eq!(
            tray_label(StagedKind::Pending, "1.3.0"),
            "Install update v1.3.0 and restart"
        );
    }

    // Composition check: `Staged::kind()`/`version()` feeding `tray_label`
    // must agree with the label the Installed variant should actually show
    // — catches a swapped kind() match arm that the two label-only tests
    // above can't. `Pending` holds a real `Update` object and isn't
    // constructible in a unit test, so only `Installed` is covered here.
    #[test]
    fn installed_staged_composes_into_restart_label() {
        let staged = Staged::Installed { version: "1.3.0".into() };
        assert_eq!(
            tray_label(staged.kind(), staged.version()),
            "Restart to update to v1.3.0"
        );
    }
}
