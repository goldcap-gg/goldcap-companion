//! Tray icon + menu: a disabled status line, "Sync now", "Open Companion…",
//! and "Quit" — the entire visible surface of this app outside the
//! companion window itself.

use crate::state::AppState;
use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::TrayIcon,
    tray::TrayIconBuilder,
    AppHandle, Manager, State, WebviewUrl, WebviewWindowBuilder,
};

const STATUS_ITEM_ID: &str = "status";
const SYNC_NOW_ITEM_ID: &str = "sync_now";
const SETTINGS_ITEM_ID: &str = "settings";
const QUIT_ITEM_ID: &str = "quit";

/// Handles kept around so `refresh` can update the tray without re-querying
/// the menu tree every time.
pub struct TrayHandles {
    pub tray: TrayIcon,
    pub status_item: MenuItem<tauri::Wry>,
}

pub fn build(app: &AppHandle) -> tauri::Result<TrayHandles> {
    let status_item =
        MenuItem::with_id(app, STATUS_ITEM_ID, "not synced yet", false, None::<&str>)?;
    let sync_now_item = MenuItem::with_id(app, SYNC_NOW_ITEM_ID, "Sync now", true, None::<&str>)?;
    let settings_item =
        MenuItem::with_id(app, SETTINGS_ITEM_ID, "Open Companion…", true, None::<&str>)?;
    let quit_item = MenuItem::with_id(app, QUIT_ITEM_ID, "Quit", true, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;

    let menu = Menu::with_items(
        app,
        &[
            &status_item,
            &separator,
            &sync_now_item,
            &settings_item,
            &separator,
            &quit_item,
        ],
    )?;

    let mut builder = TrayIconBuilder::new()
        .menu(&menu)
        .tooltip("GoldCap Companion")
        .show_menu_on_left_click(true)
        .on_menu_event(on_menu_event);

    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }

    let tray = builder.build(app)?;

    Ok(TrayHandles { tray, status_item })
}

fn on_menu_event(app: &AppHandle, event: tauri::menu::MenuEvent) {
    match event.id().as_ref() {
        SYNC_NOW_ITEM_ID => {
            let state: State<AppState> = app.state();
            let _ = state.trigger_tx.try_send(());
        }
        SETTINGS_ITEM_ID => open_main_window(app),
        QUIT_ITEM_ID => app.exit(0),
        _ => {}
    }
}

/// Opens the app window, or focuses it if it's already open.
pub fn open_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.set_focus();
        return;
    }
    let result = WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
        .title("GoldCap Companion")
        .inner_size(480.0, 660.0)
        .resizable(false)
        .build();
    if let Err(e) = result {
        eprintln!("companion: failed to open the companion window: {e}");
    }
}

/// Refreshes the tray tooltip and the disabled status menu line from the
/// shared `SyncStatus`. Called right after every sync attempt and on a
/// cosmetic timer so "Nm ago" keeps advancing between syncs.
pub fn refresh(app: &AppHandle) {
    let state: State<AppState> = app.state();
    let label = {
        let status = state.status.lock().unwrap_or_else(|p| p.into_inner());
        status.label()
    };
    let handles: State<TrayHandles> = app.state();
    let _ = handles.status_item.set_text(&label);
    let _ = handles
        .tray
        .set_tooltip(Some(format!("GoldCap Companion — {label}")));
}
