//! Launch-at-startup: reconciles the OS-level registration (macOS
//! LaunchAgent, Windows registry run key) with `Config::launch_at_startup`.
//! Shared by the app's own startup (so a config that says "on" is actually
//! registered with the OS every launch, not just after the next Settings
//! save) and by `commands::save_config` (so toggling it in the UI takes
//! effect immediately).

use crate::logging::Logger;
use tauri::AppHandle;

#[cfg(any(target_os = "macos", windows, target_os = "linux"))]
pub fn apply(app: &AppHandle, logger: &Logger, enabled: bool) {
    use tauri_plugin_autostart::ManagerExt;
    let autostart = app.autolaunch();

    // Only act on an actual change. `disable()` deletes the registry value
    // outright (auto-launch's Windows impl calls `delete_value`), so calling
    // it when autostart was never enabled fails with "cannot find the file
    // specified" — every call with the toggle off would log an ERROR for a
    // no-op, and a real autostart failure would be indistinguishable from
    // that noise. Reading the current state first also spares the registry
    // a write on every save and on every app start.
    match autostart.is_enabled() {
        Ok(current) if current == enabled => return,
        Ok(_) => {}
        Err(e) => {
            // Unreadable state is itself worth knowing about; fall through
            // and let the enable/disable below report what it hits.
            logger.error(&format!("could not read launch-at-startup state: {e}"));
        }
    }

    let result = if enabled {
        autostart.enable()
    } else {
        autostart.disable()
    };
    if let Err(e) = result {
        logger.error(&format!("failed to update launch-at-startup: {e}"));
    }
}

#[cfg(not(any(target_os = "macos", windows, target_os = "linux")))]
pub fn apply(_app: &AppHandle, _logger: &Logger, _enabled: bool) {}
