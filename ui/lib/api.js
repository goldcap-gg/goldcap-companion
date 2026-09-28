// The only module that touches window.__TAURI__. Everything else imports
// named functions from here, which is also what lets dev.html swap the whole
// backend for fixtures with one assignment.

const invoke = (cmd, args) => window.__TAURI__.core.invoke(cmd, args);

export const getConfig = () => invoke("get_config");
export const saveConfig = (config) => invoke("save_config", { config });
export const getStatus = () => invoke("get_status");
export const syncNow = () => invoke("sync_now");
/** The one setup-complete rule, computed in Rust — see config.rs's `is_complete`. */
export const setupComplete = () => invoke("setup_complete");
export const detectWowPath = () => invoke("detect_wow_path");
export const pickWowPath = () => invoke("pick_wow_path");
/** The WoW root folder (holds `_retail_`, `_classic_beta_`, …), not `_retail_` itself. */
export const detectWowRoot = () => invoke("detect_wow_root");
export const pickWowRoot = () => invoke("pick_wow_root");
/** Which games exist under a WoW root, for the wizard's "Which WoW do you play?" step. */
export const detectGames = (root) => invoke("detect_games", { root });
export const detectGame = (wowRetailPath) => invoke("detect_game", { wowRetailPath });
export const resolveRealm = (region, name) => invoke("resolve_realm", { region, name });
/** Every realm in a region, so the settings screen can offer the list. */
export const listRegionRealms = (region) => invoke("list_region_realms", { region });
export const pairWithCode = (code) => invoke("pair_with_code", { code });
export const unpair = () => invoke("unpair");
export const openAccountPage = () => invoke("open_account_page");
/** The update waiting to be applied, or null. See lib/updateBanner.js. */
export const updateReady = () => invoke("update_ready");
export const installUpdate = () => invoke("install_update");
/** Check right now; the update that is waiting, or null when up to date. */
export const checkForUpdates = () => invoke("check_for_updates");
