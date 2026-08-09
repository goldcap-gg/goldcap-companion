// The only module that touches window.__TAURI__. Everything else imports
// named functions from here, which is also what lets dev.html swap the whole
// backend for fixtures with one assignment.

const invoke = (cmd, args) => window.__TAURI__.core.invoke(cmd, args);

export const getConfig = () => invoke("get_config");
export const saveConfig = (config) => invoke("save_config", { config });
export const getStatus = () => invoke("get_status");
export const syncNow = () => invoke("sync_now");
export const detectWowPath = () => invoke("detect_wow_path");
export const pickWowPath = () => invoke("pick_wow_path");
export const detectGame = (wowRetailPath) => invoke("detect_game", { wowRetailPath });
export const resolveRealm = (region, name) => invoke("resolve_realm", { region, name });
export const pairWithCode = (code) => invoke("pair_with_code", { code });
export const unpair = () => invoke("unpair");
export const openAccountPage = () => invoke("open_account_page");
