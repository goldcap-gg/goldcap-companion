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

/// Emitted when an update becomes available, so an open window can offer it.
pub const UPDATE_STAGED_EVENT: &str = "update-staged";

/// Don't compete with the first addon sync right after startup.
pub const CHECK_STARTUP_DELAY: Duration = Duration::from_secs(60);
pub const CHECK_INTERVAL: Duration = Duration::from_secs(4 * 60 * 60);

/// How far a found update has progressed on this platform.
#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum StagedKind {
    /// Already written to disk; a restart applies it (macOS).
    Installed,
    /// Downloaded, waiting for the user to allow the installer run (Windows).
    Pending,
    /// We found it but could not stage it — an install that failed (no write
    /// access to the .app, a quarantined copy, a locked installer). Silence
    /// here is the worst outcome: the app would sit on an old version
    /// forever with nothing on screen ever saying so. Offer the download
    /// page instead, which always works.
    Manual,
}

/// Tray menu label for a staged update.
pub fn tray_label(kind: StagedKind, version: &str) -> String {
    match kind {
        StagedKind::Installed => format!("Restart to update to v{version}"),
        StagedKind::Pending => format!("Install update v{version} and restart"),
        StagedKind::Manual => format!("Download update v{version}…"),
    }
}

/// What the button in the window should say. The tray label is written for a
/// menu ("Restart to update to v1.6.0"); a button next to a sentence that
/// already names the version wants fewer words.
pub fn action_label(kind: StagedKind) -> &'static str {
    match kind {
        StagedKind::Installed => "Restart now",
        StagedKind::Pending => "Install and restart",
        StagedKind::Manual => "Open downloads",
    }
}

/// The staged update as the frontend sees it. Emitted on `update-staged` and
/// returned by the `update_ready` command, so a window that opens later shows
/// the same offer as one that was already open.
#[derive(Clone, serde::Serialize)]
pub struct UpdateView {
    pub version: String,
    pub kind: StagedKind,
    pub action: &'static str,
}

pub fn view(kind: StagedKind, version: &str) -> UpdateView {
    UpdateView { version: version.to_string(), kind, action: action_label(kind) }
}

/// A found update, staged as far as this platform allows.
pub enum Staged {
    Installed { version: String },
    /// Found, but this machine wouldn't let us apply it — see StagedKind::Manual.
    Manual { version: String },
    // Kept in memory (NSIS installer, a few MB) so the click handler can
    // run it without a re-download; dropped if the app quits first, and
    // the next startup check simply re-finds the same version.
    #[cfg_attr(not(windows), allow(dead_code))]
    Pending {
        version: String,
        // Boxed: the other variants are a string, and an unboxed Update
        // would size every Staged value to the largest one (clippy's
        // large_enum_variant).
        update: Box<tauri_plugin_updater::Update>,
        bytes: Vec<u8>,
    },
}

impl Staged {
    pub fn kind(&self) -> StagedKind {
        match self {
            Staged::Installed { .. } => StagedKind::Installed,
            Staged::Manual { .. } => StagedKind::Manual,
            Staged::Pending { .. } => StagedKind::Pending,
        }
    }
    pub fn version(&self) -> &str {
        match self {
            Staged::Installed { version }
            | Staged::Manual { version }
            | Staged::Pending { version, .. } => version,
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
        // macOS: extract over the .app now; takes effect on relaunch. A
        // failure here used to return early and leave NOTHING on screen —
        // the app then sat on the old version indefinitely, silently, which
        // is exactly how a running companion misses release after release.
        // Now it degrades to an offer the user can act on.
        match update.install(bytes) {
            Ok(()) => {
                logger.info(&format!("update v{version} installed, restart to apply"));
                Staged::Installed { version }
            }
            Err(e) => {
                logger.error(&format!("update install failed, offering the download page: {e}"));
                Staged::Manual { version }
            }
        }
    };
    #[cfg(windows)]
    let staged = {
        logger.info(&format!("update v{version} downloaded, waiting for restart"));
        Staged::Pending { version, update: Box::new(update), bytes }
    };

    let label = tray_label(staged.kind(), staged.version());
    let announcement = view(staged.kind(), staged.version());
    *state.staged.lock().unwrap_or_else(|p| p.into_inner()) = Some(staged);
    tray::show_update_item(app, &label);
    announce(app, &announcement, logger);
    Ok(())
}

/// Say it where someone will see it.
///
/// A tray menu item is only visible to someone who opens the tray menu, and
/// nobody opens the menu of a tray app that is working — which is why a
/// staged update could sit unnoticed for weeks. So: an OS notification (the
/// app may have no window open at all) and an event any open window turns
/// into a banner with a button.
fn announce(app: &AppHandle, update: &UpdateView, logger: &Logger) {
    use tauri::Emitter;
    use tauri_plugin_notification::NotificationExt;

    if let Err(e) = app.emit(UPDATE_STAGED_EVENT, update.clone()) {
        logger.error(&format!("could not tell the window about the update: {e}"));
    }

    let body = match update.kind {
        StagedKind::Manual => format!(
            "Version {} is out, but this copy could not update itself. Open the downloads page to get it.",
            update.version
        ),
        _ => format!("Version {} is ready — {}.", update.version, update.action.to_lowercase()),
    };
    // Best effort by design: notification permission can be denied at the OS
    // level, and a missing toast must not cost the tray item or the banner.
    if let Err(e) = app
        .notification()
        .builder()
        .title("GoldCap Companion update")
        .body(body)
        .show()
    {
        logger.error(&format!("update notification failed: {e}"));
    }
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
        Some(Staged::Manual { version }) => {
            use tauri_plugin_opener::OpenerExt;
            // Keep it staged: opening a page doesn't update anything, so the
            // offer must survive the click.
            *state.staged.lock().unwrap_or_else(|p| p.into_inner()) =
                Some(Staged::Manual { version });
            let _ = app
                .opener()
                .open_url("https://goldcap.gg/downloads/", None::<&str>);
        }
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

    // The failure this whole change exists for: an update that could not be
    // installed must still become an OFFER, never silence.
    #[test]
    fn manual_staging_offers_the_download_page() {
        let staged = Staged::Manual { version: "1.6.0".into() };
        assert_eq!(staged.kind(), StagedKind::Manual);
        assert_eq!(tray_label(staged.kind(), staged.version()), "Download update v1.6.0…");
        assert_eq!(action_label(staged.kind()), "Open downloads");
    }

    #[test]
    fn every_kind_has_its_own_button_label() {
        let labels = [
            action_label(StagedKind::Installed),
            action_label(StagedKind::Pending),
            action_label(StagedKind::Manual),
        ];
        let mut unique = labels.to_vec();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), labels.len(), "two kinds share a button label: {labels:?}");
    }

    // What the window renders comes from `view`, so it has to carry the same
    // version and action the tray does — a banner offering "Restart now" for
    // an update that was never installed would be a lie.
    #[test]
    fn view_matches_the_staged_update() {
        let staged = Staged::Manual { version: "1.6.0".into() };
        let v = view(staged.kind(), staged.version());
        assert_eq!(v.version, "1.6.0");
        assert_eq!(v.action, "Open downloads");
        assert_eq!(
            serde_json::to_value(v.kind).unwrap(),
            serde_json::json!("manual"),
            "the frontend switches on this string",
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
