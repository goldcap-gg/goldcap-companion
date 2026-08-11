// Tray-only companion app: no console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod autostart;
mod commands;
mod config;
mod health;
mod logging;
mod luafile;
mod savedvars;
mod state;
mod status;
mod sync;
mod tray;
mod upload;
mod watcher;
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
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            commands::get_config,
            commands::save_config,
            commands::sync_now,
            commands::get_status,
            commands::detect_wow_path,
            commands::pick_wow_path,
            commands::detect_game,
            commands::resolve_realm,
            commands::pair_with_code,
            commands::unpair,
            commands::open_account_page,
        ])
        .setup(|app| {
            let handle = app.handle().clone();

            let config_dir = handle.path().app_config_dir()?;
            std::fs::create_dir_all(&config_dir)?;
            let config_path = config_dir.join(config::CONFIG_FILE_NAME);

            let logger = Arc::new(logging::Logger::new(&config_dir)?);
            let initial_config = Config::load_or_init(&config_path)?;
            let config_for_first_run = initial_config.clone();
            logger.info("companion starting");

            // Registers (or unregisters) the OS-level login item on every
            // start, not just after a Settings save — a fresh install's
            // default config already says `launchAtStartup: true`, and
            // without this it would never actually be registered until the
            // user happened to open Settings and hit save once.
            autostart::apply(&handle, &logger, initial_config.launch_at_startup);

            let (config_tx, config_rx) = watch::channel(initial_config.clone());
            let (trigger_tx, trigger_rx) = mpsc::channel::<()>(4);
            let status = Arc::new(Mutex::new(sync::SyncStatus::default()));

            // Counters live on disk next to the dedupe keys; without this the
            // Status screen would show "0 rows sent" until the first upload of
            // every session.
            {
                let persisted = upload::UploadState::load_from(
                    &config_dir.join(upload::STATE_FILE_NAME),
                );
                status.lock().unwrap_or_else(|p| p.into_inner()).upload =
                    persisted.stats().clone();
            }

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

            // A tray-only app has nowhere to put a first-run wizard: without
            // this, a brand-new install shows an icon and nothing else.
            if !config_for_first_run.is_complete() {
                tray::open_main_window(&handle);
            }

            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building the goldcap companion app")
        .run(|_app_handle, event| {
            // Tray-only app: closing the companion window must not quit the
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
