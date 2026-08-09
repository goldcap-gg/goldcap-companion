//! Tauri commands invoked from the Settings window.

use crate::config::{self, Config};
use crate::state::AppState;
use crate::wtf;
use std::path::Path;
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

/// Backs the Settings window's "Browse…" button: a native folder picker.
/// Accepts either the WoW base dir or `_retail_` itself and hands back the
/// normalized `_retail_` path (empty answer = user cancelled; an error =
/// the picked folder is not a WoW retail install).
#[tauri::command]
pub async fn pick_wow_path(app: AppHandle) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;

    // Folder pickers must not block the main thread — run the blocking
    // variant on the runtime's blocking pool.
    let picked =
        tauri::async_runtime::spawn_blocking(move || app.dialog().file().blocking_pick_folder())
            .await
            .map_err(|e| e.to_string())?;

    let Some(folder) = picked else {
        return Ok(None); // cancelled
    };
    let path = folder.into_path().map_err(|e| e.to_string())?;
    match config::normalize_retail_dir(&path) {
        Some(retail) => Ok(Some(retail.to_string_lossy().into_owned())),
        None => Err(format!(
            "No _retail_ folder found under {} — pick the World of Warcraft install folder",
            path.display()
        )),
    }
}

/// Reads region + realm names out of the WoW client's own files (Config.wtf
/// portal cvar, WTF/Account realm folders) so Settings can prefill both.
#[tauri::command]
pub fn detect_game(wow_retail_path: String) -> wtf::GameSettings {
    wtf::read_game_settings(Path::new(&wow_retail_path))
}

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedRealm {
    pub slug: String,
    pub name_en: String,
}

/// Resolves a realm display name (any locale, member or leader name) to the
/// connected-realm slug the sync loop needs, via the public
/// `/v1/addon/resolve-realm` endpoint.
#[tauri::command]
pub async fn resolve_realm(region: String, name: String) -> Result<ResolvedRealm, String> {
    let client = crate::sync::build_client();
    let resp = client
        .get("https://api.goldcap.gg/v1/addon/resolve-realm")
        .query(&[("region", region.as_str()), ("name", name.as_str())])
        .send()
        .await
        .map_err(|e| e.to_string())?;

    match resp.status().as_u16() {
        200 => resp
            .json::<ResolvedRealm>()
            .await
            .map_err(|e| e.to_string()),
        404 => Err(format!("realm \"{name}\" not found on {region}")),
        code => Err(format!("resolve failed: HTTP {code}")),
    }
}

/// The whole pipeline as data, for the Status screen. The tray menu renders
/// its own single line straight from `SyncStatus::label`.
#[tauri::command]
pub fn get_status(state: State<AppState>) -> crate::status::StatusSnapshot {
    let config = state
        .config
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone();
    let status = state
        .status
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone();
    let health = crate::health::inspect(Path::new(&config.wow_retail_path));
    crate::status::build(&config, &status, &health, crate::luafile::now_unix())
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

/// Trades a pairing code from goldcap.gg/account for a long-lived upload
/// token and stores it in the config. Reuses save_config so the running sync
/// loop picks the token up on its next tick without a restart.
#[tauri::command]
pub async fn pair_with_code(
    app: AppHandle,
    state: State<'_, AppState>,
    code: String,
) -> Result<(), String> {
    let trimmed = code.trim().to_string();
    if trimmed.is_empty() {
        return Err("enter the code from goldcap.gg/account".into());
    }

    // Label the pairing with the machine name so a player with several PCs can
    // tell them apart when revoking one later.
    let label = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "companion".to_string());

    let client = reqwest::Client::new();
    let token = crate::upload::claim_code(&client, &trimmed, &label).await?;

    let mut config = state.config.lock().unwrap_or_else(|p| p.into_inner()).clone();
    config.companion_token = token;
    save_config(app, state, config)
}

/// Whether this companion is paired. The token itself is never handed back to
/// the UI — there is nothing the settings window could do with it except leak
/// it into a screenshot.
#[tauri::command]
pub fn is_paired(state: State<AppState>) -> bool {
    !state
        .config
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .companion_token
        .trim()
        .is_empty()
}

/// Drops the upload token. Goes through `save_config` so the running sync
/// loop stops uploading on its next tick without a restart.
#[tauri::command]
pub fn unpair(app: AppHandle, state: State<AppState>) -> Result<(), String> {
    let mut config = state
        .config
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone();
    config.companion_token = String::new();
    save_config(app, state, config)
}

/// Opens the page that issues pairing codes in the user's browser. The URL
/// is hard-coded and this command takes no argument, so nothing the webview
/// sends can change where it goes — that, not the capability scope, is what
/// makes it safe. `OpenerExt::open_url` is an in-process call and never
/// reaches Tauri's ACL; the scoped `opener:allow-open-url` entry in
/// capabilities/default.json constrains only direct frontend calls to the
/// plugin's own IPC command, which nothing here makes.
#[tauri::command]
pub fn open_account_page(app: AppHandle) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    app.opener()
        .open_url("https://goldcap.gg/account", None::<&str>)
        .map_err(|e| e.to_string())
}
