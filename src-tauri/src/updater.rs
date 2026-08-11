//! Auto-update: silent periodic check + download, tray-driven restart.
//!
//! Platform split (spec 2026-08-11): macOS installs immediately (the
//! extracted .app applies on next launch) and the tray offers a restart;
//! Windows can't install without NSIS killing the app, so the downloaded
//! installer is staged in memory and only runs when the user clicks the
//! tray item.

use std::time::Duration;

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
}
