//! Shared application state, managed via `tauri::App::manage`.

use crate::config::Config;
use crate::logging::Logger;
use crate::sync::SyncStatus;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, watch};

pub struct AppState {
    /// In-memory mirror of the on-disk config, kept in sync with `config_tx`.
    pub config: Mutex<Config>,
    pub config_path: PathBuf,
    pub status: Arc<Mutex<SyncStatus>>,
    pub logger: Arc<Logger>,
    /// Fires one immediate sync attempt ("Sync now" / the tray menu item).
    pub trigger_tx: mpsc::Sender<()>,
    /// Publishes config changes to the sync loop, which restarts its
    /// interval timer with the new `intervalMinutes` on every update.
    pub config_tx: watch::Sender<Config>,
}
