// Tray-only companion app: no console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod config;
mod health;
mod logging;
mod luafile;
mod savedvars;
mod state;
mod sync;
mod tray;
mod upload;
mod wtf;

use config::Config;
use state::AppState;
use std::sync::{Arc, Mutex};
use tauri::Manager;
use tokio::sync::{mpsc, watch};

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            commands::get_config,
            commands::save_config,
            commands::sync_now,
            commands::get_status_label,
            commands::detect_wow_path,
            commands::pick_wow_path,
            commands::detect_game,
            commands::resolve_realm,
            commands::pair_with_code,
            commands::is_paired,
        ])
        .setup(|app| {
            let handle = app.handle().clone();

            let config_dir = handle.path().app_config_dir()?;
            std::fs::create_dir_all(&config_dir)?;
            let config_path = config_dir.join(config::CONFIG_FILE_NAME);

            let logger = Arc::new(logging::Logger::new(&config_dir)?);
            let initial_config = Config::load_or_init(&config_path)?;
            logger.info("companion starting");

            let (config_tx, config_rx) = watch::channel(initial_config.clone());
            let (trigger_tx, trigger_rx) = mpsc::channel::<()>(4);
            let status = Arc::new(Mutex::new(sync::SyncStatus::default()));

            let tray_handles = tray::build(&handle)?;

            app.manage(AppState {
                config: Mutex::new(initial_config),
                config_path,
                status: status.clone(),
                logger: logger.clone(),
                trigger_tx,
                config_tx,
            });
            app.manage(tray_handles);

            let client = sync::build_client();
            let sync_refresh_handle = handle.clone();
            tauri::async_runtime::spawn(sync::run_loop(
                client,
                config_rx,
                trigger_rx,
                status,
                logger,
                config_dir.clone(),
                move || tray::refresh(&sync_refresh_handle),
            ));

            // Cosmetic ticker: keeps the "Nm ago" status label advancing
            // between actual sync attempts.
            let cosmetic_refresh_handle = handle.clone();
            tauri::async_runtime::spawn(async move {
                let mut ticker = tokio::time::interval(std::time::Duration::from_secs(30));
                loop {
                    ticker.tick().await;
                    tray::refresh(&cosmetic_refresh_handle);
                }
            });

            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building the goldcap companion app")
        .run(|_app_handle, event| {
            // Tray-only app: closing the Settings window must not quit the
            // process. A window-close exit request carries code None; an
            // explicit app.exit(0) (the tray's Quit item) carries Some(0)
            // and must be allowed through — blocking unconditionally here
            // is exactly the "Quit does nothing" bug.
            if let tauri::RunEvent::ExitRequested { api, code, .. } = event {
                if code.is_none() {
                    api.prevent_exit();
                }
            }
        });
}
