// Tray-only companion app: no console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod config;
mod logging;
mod luafile;
mod state;
mod sync;
mod tray;

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
        .invoke_handler(tauri::generate_handler![
            commands::get_config,
            commands::save_config,
            commands::sync_now,
            commands::get_status_label,
            commands::detect_wow_path,
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
            // process — only the tray menu's "Quit" item does that.
            if let tauri::RunEvent::ExitRequested { api, .. } = event {
                api.prevent_exit();
            }
        });
}
