//! Tauri commands invoked from the Settings window.

use crate::config::{self, Config};
use crate::state::AppState;
use tauri::{AppHandle, State};

#[tauri::command]
pub fn get_config(state: State<AppState>) -> Config {
    state
        .config
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone()
}

/// Backs the Settings window's "Detect" button — re-runs the same
/// auto-detect used on first run, without touching the saved config.
#[tauri::command]
pub fn detect_wow_path() -> String {
    config::detect_wow_retail_path()
}

#[tauri::command]
pub fn get_status_label(state: State<AppState>) -> String {
    state
        .status
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .label()
}

#[tauri::command]
pub fn sync_now(state: State<AppState>) -> Result<(), String> {
    state.trigger_tx.try_send(()).map_err(|e| e.to_string())
}

/// Persists `config`, applies the launch-at-startup toggle, and republishes
/// it to the running sync loop so a new `intervalMinutes` (or realm/region)
/// takes effect without restarting the app.
#[tauri::command]
pub fn save_config(app: AppHandle, state: State<AppState>, config: Config) -> Result<(), String> {
    config
        .save_to(&state.config_path)
        .map_err(|e| e.to_string())?;
    apply_autostart(&app, &state, config.launch_at_startup);

    *state.config.lock().unwrap_or_else(|p| p.into_inner()) = config.clone();
    let _ = state.config_tx.send(config);
    state.logger.info("config saved");
    Ok(())
}

#[cfg(any(target_os = "macos", windows, target_os = "linux"))]
fn apply_autostart(app: &AppHandle, state: &State<AppState>, enabled: bool) {
    use tauri_plugin_autostart::ManagerExt;
    let autostart = app.autolaunch();
    let result = if enabled {
        autostart.enable()
    } else {
        autostart.disable()
    };
    if let Err(e) = result {
        state
            .logger
            .error(&format!("failed to update launch-at-startup: {e}"));
    }
}

#[cfg(not(any(target_os = "macos", windows, target_os = "linux")))]
fn apply_autostart(_app: &AppHandle, _state: &State<AppState>, _enabled: bool) {}
