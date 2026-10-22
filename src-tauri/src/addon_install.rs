use crate::addon_files::{recover, staging_dir, swap_steps, unpack};
use crate::addon_folder::{decide, inspect, toc_version, Decision, FolderState};
use crate::addon_game::{wow_is_running, GameProcesses, SystemProcesses};
use crate::addon_manifest::{
    check_payload, updater_pubkey, verify_manifest, AddonManifest, Version, VerifiedRelease, MANIFEST_URL,
    URL_PREFIX,
};
use crate::config::Config;
use crate::games::{detect_games, forever_root, DetectedKind, GameKind};
use crate::logging::Logger;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Manager};

pub const STATE_FILE_NAME: &str = "addon-state.json";
pub const STARTUP_DELAY: Duration = Duration::from_secs(10);
/// How often to look again while an update waits for the game to close.
const WAITING_INTERVAL: Duration = Duration::from_secs(2 * 60);
/// The release zips are ~2 MB; a manifest that names more than this is wrong.
const MAX_ZIP_BYTES: u64 = 32 * 1024 * 1024;
const ZIP_TIMEOUT: Duration = Duration::from_secs(120);

/// One game folder whose `Interface/AddOns` the addon goes into.
#[derive(Debug, Clone, PartialEq)]
pub struct Target {
    pub game: GameKind,
    pub folder: String,
    pub game_dir: PathBuf,
}

impl Target {
    pub fn addons_dir(&self) -> PathBuf {
        self.game_dir.join("Interface").join("AddOns")
    }
}

pub fn targets(config: &Config) -> Vec<Target> {
    let mut out = Vec::new();
    let retail = config.wow_retail_path.trim();
    if config.retail_enabled && !retail.is_empty() && Path::new(retail).is_dir() {
        out.push(Target {
            game: GameKind::Retail,
            folder: "_retail_".to_string(),
            game_dir: PathBuf::from(retail),
        });
    }
    if config.forever_enabled {
        if let Some(root) = forever_root(config) {
            for g in detect_games(&root) {
                if g.kind == DetectedKind::Forever {
                    out.push(Target { game: GameKind::Forever, game_dir: root.join(&g.folder), folder: g.folder });
                }
            }
        }
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LineKind {
    Off,
    Managed,
    Foreign,
    TakenOver,
    Waiting,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Change {
    Installed,
    Updated,
}

/// What the status window shows for one game install. `dir` is the AddOns
/// folder and the key the take-over button hands back.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddonLine {
    pub dir: String,
    pub folder: String,
    pub game: GameKind,
    pub kind: LineKind,
    pub installed_version: Option<String>,
    pub available_version: Option<String>,
    pub updated_at: Option<i64>,
    pub last_change: Option<Change>,
    pub error: Option<String>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddonState {
    /// AddOns folders whose `GoldCap` the player handed over.
    #[serde(default)]
    pub taken_over: BTreeSet<String>,
    #[serde(default)]
    pub lines: BTreeMap<String, AddonLine>,
}

impl AddonState {
    /// A missing or unreadable file is a fresh start.
    pub fn load_from(path: &Path) -> AddonState {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn save_to(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        crate::luafile::write_atomic(path, &text)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Apply(Decision),
    Skip(LineKind),
}

pub fn plan(state: &FolderState, taken_over: bool, available: &Version, game_running: bool) -> Action {
    match decide(state, taken_over, available) {
        Decision::Install | Decision::Update if game_running => Action::Skip(LineKind::Waiting),
        d @ (Decision::Install | Decision::Update) => Action::Apply(d),
        Decision::LeaveAlone => Action::Skip(LineKind::Foreign),
        Decision::TakenOverWaiting => Action::Skip(LineKind::TakenOver),
        Decision::UpToDate => Action::Skip(LineKind::Managed),
    }
}

#[derive(Default)]
pub struct AddonControl {
    /// Held for a whole run: the loop, the wizard and the Settings box can land together.
    running: tokio::sync::Mutex<()>,
    /// A verified download kept while the game is open.
    pending: Mutex<Option<VerifiedRelease>>,
    pub wake: tokio::sync::Notify,
}

pub struct Ctx<'a> {
    pub client: &'a reqwest::Client,
    pub procs: &'a dyn GameProcesses,
    pub control: &'a AddonControl,
    pub logger: &'a Logger,
}

pub struct Source<'a> {
    pub manifest_url: &'a str,
    /// Where a zip named by the manifest has to live.
    pub url_prefix: &'a str,
    pub pubkey: &'a str,
}

/// The wait before the next pass. An update waiting for the game is looked at
/// again in two minutes. So is a failed one: mostly a file an antivirus scan or
/// an Explorer window held for a moment, or a dropped connection. Each failed
/// pass in a row doubles that wait, up to the normal cycle, so a release that
/// keeps failing is not downloaded again every two minutes.
pub fn next_wait(lines: &[AddonLine], failed_in_a_row: u32) -> Duration {
    if lines.iter().any(|l| l.kind == LineKind::Waiting) {
        return WAITING_INTERVAL;
    }
    if lines.iter().any(|l| l.kind == LineKind::Failed) {
        let doublings = failed_in_a_row.saturating_sub(1).min(16);
        return (WAITING_INTERVAL * (1u32 << doublings)).min(crate::updater::CHECK_INTERVAL);
    }
    crate::updater::CHECK_INTERVAL
}

async fn fetch(client: &reqwest::Client, url: &str, timeout: Option<Duration>) -> Result<Vec<u8>, String> {
    let mut req = client.get(url).header("Cache-Control", "no-cache");
    if let Some(t) = timeout {
        req = req.timeout(t);
    }
    let limit = timeout.unwrap_or(crate::sync::CLIENT_TIMEOUT);
    let resp = req.send().await.map_err(|e| crate::sync::describe_err(&e, limit))?;
    if !resp.status().is_success() {
        return Err(format!("{url} answered {}", resp.status()));
    }
    resp.bytes().await.map(|b| b.to_vec()).map_err(|e| crate::sync::describe_err(&e, limit))
}

async fn fetch_manifest(ctx: &Ctx<'_>, src: &Source<'_>) -> Result<(AddonManifest, Version), String> {
    let json = fetch(ctx.client, src.manifest_url, None).await?;
    let sig = fetch(ctx.client, &format!("{}.sig", src.manifest_url), None).await?;
    let sig = String::from_utf8(sig).map_err(|_| "signature is not text".to_string())?;
    verify_manifest(&json, &sig, src.pubkey, src.url_prefix)
}

/// The zip lives beside the manifest. Production's manifest sits at the
/// prefix, so this is the manifest's own `url`.
async fn fetch_release(
    ctx: &Ctx<'_>,
    src: &Source<'_>,
    manifest: &AddonManifest,
    version: Version,
) -> Result<VerifiedRelease, String> {
    if manifest.size > MAX_ZIP_BYTES {
        return Err(format!("manifest names a {} byte zip", manifest.size));
    }
    let dir = &src.manifest_url[..src.manifest_url.rfind('/').map_or(0, |i| i + 1)];
    let url = format!("{dir}{}", &manifest.url[src.url_prefix.len()..]);
    let zip = fetch(ctx.client, &url, Some(ZIP_TIMEOUT)).await?;
    check_payload(&zip, manifest)?;
    Ok(VerifiedRelease { manifest: manifest.clone(), version, zip })
}

fn installed_version(state: &FolderState) -> Option<String> {
    match state {
        FolderState::Ours(v) | FolderState::Foreign(Some(v)) => Some(v.to_string()),
        _ => None,
    }
}

fn base_line(t: &Target, kind: LineKind, fs: &FolderState, available: Option<&Version>, prev: Option<&AddonLine>) -> AddonLine {
    AddonLine {
        dir: t.addons_dir().to_string_lossy().to_string(),
        folder: t.folder.clone(),
        game: t.game,
        kind,
        installed_version: installed_version(fs),
        available_version: available.map(|v| v.to_string()),
        updated_at: prev.and_then(|p| p.updated_at),
        last_change: prev.and_then(|p| p.last_change),
        error: None,
    }
}

/// One pass with the production manifest and key.
pub async fn run_once(ctx: &Ctx<'_>, config: &Config, state_path: &Path, now: i64) -> Vec<AddonLine> {
    let src = Source { manifest_url: MANIFEST_URL, url_prefix: URL_PREFIX, pubkey: updater_pubkey() };
    run_once_with_key(ctx, &src, config, state_path, now).await
}

pub(crate) async fn run_once_with_key(
    ctx: &Ctx<'_>,
    src: &Source<'_>,
    config: &Config,
    state_path: &Path,
    now: i64,
) -> Vec<AddonLine> {
    let _serial = ctx.control.running.lock().await;
    let mut state = AddonState::load_from(state_path);
    let targets = targets(config);
    for t in &targets {
        recover(&t.addons_dir());
    }
    let key = |t: &Target| t.addons_dir().to_string_lossy().to_string();

    if !config.addon_install {
        let lines: Vec<AddonLine> = targets
            .iter()
            .map(|t| base_line(t, LineKind::Off, &FolderState::Foreign(None), None, None))
            .collect();
        state.lines = lines.iter().map(|l| (l.dir.clone(), l.clone())).collect();
        save(&state, state_path, ctx.logger);
        return lines;
    }

    let (manifest, available) = match fetch_manifest(ctx, src).await {
        Ok(ok) => ok,
        Err(e) => {
            ctx.logger.error(&format!("addon: manifest not used: {e}"));
            return state.lines.values().cloned().collect();
        }
    };

    let folders: Vec<FolderState> = targets.iter().map(|t| inspect(&t.addons_dir())).collect();
    let taken = |t: &Target| state.taken_over.contains(&key(t));
    let wanted = targets
        .iter()
        .zip(&folders)
        .any(|(t, f)| matches!(plan(f, taken(t), &available, false), Action::Apply(_)));

    // Download first, so an update waiting for the game is already on disk.
    let mut release: Option<VerifiedRelease> = None;
    let mut download_error: Option<String> = None;
    if wanted {
        let kept = ctx
            .control
            .pending
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
            .filter(|r| r.manifest == manifest);
        match kept {
            Some(r) => release = Some(r),
            None => match fetch_release(ctx, src, &manifest, available).await {
                Ok(r) => {
                    *ctx.control.pending.lock().unwrap_or_else(|p| p.into_inner()) = Some(r.clone());
                    release = Some(r);
                }
                Err(e) => {
                    ctx.logger.error(&format!("addon: download not used: {e}"));
                    download_error = Some(e);
                }
            },
        }
    }
    let ready = wanted && download_error.is_none();
    let listing = if ready { wow_is_running(ctx.procs) } else { Ok(false) };
    let game_running = listing != Ok(false);
    // Why the game cannot be checked, shown on a card that waits because of it.
    let cannot_tell = listing.err();

    let mut lines = Vec::new();
    let mut applied = Vec::new();
    // Real paths of the AddOns folders this pass put the release in.
    let mut in_place: Vec<PathBuf> = Vec::new();
    for (t, folder_state) in targets.iter().zip(&folders) {
        let prev = state.lines.get(&key(t)).cloned();
        // A client the name list does not know still holds its exe in this install.
        let held = (ready && !game_running && matches!(plan(folder_state, taken(t), &available, false), Action::Apply(_)))
            .then(|| ctx.procs.exe_in_use(&t.game_dir))
            .flatten();
        let line = match plan(folder_state, taken(t), &available, game_running || held.is_some()) {
            Action::Skip(kind) => {
                match kind {
                    LineKind::Foreign => ctx.logger.info(&format!(
                        "addon: {} GoldCap is kept by another app, leaving it",
                        t.folder
                    )),
                    LineKind::Waiting => ctx.logger.info(&format!(
                        "addon: {} update {available} waits for the game to close{}",
                        t.folder,
                        held.as_deref().or(cannot_tell.as_deref()).map(|h| format!(" ({h})")).unwrap_or_default()
                    )),
                    _ => {}
                }
                let mut l = base_line(t, kind, folder_state, Some(&available), prev.as_ref());
                if kind == LineKind::Waiting {
                    l.error = cannot_tell.clone();
                }
                l
            }
            Action::Apply(decision) => match (&release, &download_error) {
                (Some(r), None) if already_in_place(t, r, &in_place) => {
                    // This install reaches the AddOns folder another one just got
                    // the release in, through a link: nothing is left to do.
                    ctx.logger.info(&format!(
                        "addon: {} GoldCap {} is in place, its AddOns folder is shared with another install",
                        t.folder, r.version
                    ));
                    applied.push(key(t));
                    managed_line(t, folder_state, &available, prev.as_ref(), decision, r, now)
                }
                (Some(r), None) if changed_since_checked(ctx, t, folder_state) => {
                    // The download can take minutes: a game that started or a folder
                    // another installer touched meanwhile means nothing is swapped now.
                    // The verified download stays in `pending` for the next pass.
                    ctx.logger.info(&format!(
                        "addon: {} the game or the GoldCap folder changed while the update was downloading, keeping {} staged for the next pass",
                        t.folder, r.version
                    ));
                    base_line(t, LineKind::Waiting, folder_state, Some(&available), prev.as_ref())
                }
                (Some(r), None) => {
                    match apply(t, r, now) {
                        Ok(()) => {
                            ctx.logger.info(&format!("addon: {} GoldCap {} is in place", t.folder, r.version));
                            applied.push(key(t));
                            if let Ok(real) = t.addons_dir().canonicalize() {
                                in_place.push(real);
                            }
                            managed_line(t, folder_state, &available, prev.as_ref(), decision, r, now)
                        }
                        Err(e) => {
                            ctx.logger.error(&format!("addon: {} GoldCap not changed: {e}", t.folder));
                            let mut l = base_line(t, LineKind::Failed, folder_state, Some(&available), prev.as_ref());
                            l.error = Some(e);
                            l
                        }
                    }
                }
                // The zip was not downloaded or did not match the signed manifest.
                // A manifest that cannot be read stays silent, but a player
                // waiting for an install or update is told why it did not happen.
                (_, err) => {
                    let mut l = base_line(t, LineKind::Failed, folder_state, Some(&available), prev.as_ref());
                    l.error = Some(err.clone().unwrap_or_else(|| "the download is not available".to_string()));
                    l
                }
            },
        };
        lines.push(line);
    }

    for dir in applied {
        state.taken_over.remove(&dir);
    }
    // A failed swap is tried again with the same verified download.
    if !lines.iter().any(|l| matches!(l.kind, LineKind::Waiting | LineKind::Failed)) {
        *ctx.control.pending.lock().unwrap_or_else(|p| p.into_inner()) = None;
    }
    state.lines = lines.iter().map(|l| (l.dir.clone(), l.clone())).collect();
    save(&state, state_path, ctx.logger);
    lines
}

/// True when `t`'s AddOns folder is, through a link, one this pass already put
/// `r` in, and it still holds exactly that.
fn already_in_place(t: &Target, r: &VerifiedRelease, in_place: &[PathBuf]) -> bool {
    t.addons_dir().canonicalize().is_ok_and(|real| in_place.contains(&real))
        && inspect(&t.addons_dir()) == FolderState::Ours(r.version)
}

fn managed_line(
    t: &Target,
    folder_state: &FolderState,
    available: &Version,
    prev: Option<&AddonLine>,
    decision: Decision,
    r: &VerifiedRelease,
    now: i64,
) -> AddonLine {
    let mut l = base_line(t, LineKind::Managed, folder_state, Some(available), prev);
    l.installed_version = Some(r.version.to_string());
    l.updated_at = Some(now);
    l.last_change = Some(if decision == Decision::Install { Change::Installed } else { Change::Updated });
    l
}

/// Looked at again right before the swap: the folder and the game as they are now.
fn changed_since_checked(ctx: &Ctx<'_>, t: &Target, seen: &FolderState) -> bool {
    wow_is_running(ctx.procs) != Ok(false)
        || ctx.procs.exe_in_use(&t.game_dir).is_some()
        || inspect(&t.addons_dir()) != *seen
}

/// Unpack beside `AddOns` and swap in; the staging folder never outlives the
/// call. A failure is followed by `recover` at once: a rollback that could not
/// put the old folder back must not leave the game without `GoldCap` until the
/// next pass.
fn apply(t: &Target, release: &VerifiedRelease, now: i64) -> Result<(), String> {
    apply_steps(t, release, now, &|_| Ok(()))
}

fn apply_steps(
    t: &Target,
    release: &VerifiedRelease,
    now: i64,
    hook: &dyn Fn(crate::addon_files::SwapStep) -> std::io::Result<()>,
) -> Result<(), String> {
    let addons = t.addons_dir();
    std::fs::create_dir_all(&addons).map_err(|e| format!("could not create {}: {e}", addons.display()))?;
    let staging = staging_dir(&addons, now);
    let result = std::fs::create_dir_all(&staging)
        .map_err(|e| format!("could not create a staging folder: {e}"))
        .and_then(|_| unpack(&release.zip, &staging))
        .and_then(|_| toc_matches(&staging.join("GoldCap"), &release.version))
        .and_then(|_| swap_steps(&addons, &staging.join("GoldCap"), &release.version, now, hook));
    let _ = std::fs::remove_dir_all(&staging);
    if result.is_err() {
        recover(&addons);
    }
    result
}

/// The marker records the manifest's version and `inspect` compares it with
/// the TOC's. A release whose TOC says anything else would be installed and
/// then read as another app's copy, never to be updated again.
fn toc_matches(addon_dir: &Path, version: &Version) -> Result<(), String> {
    match toc_version(addon_dir) {
        Some(v) if v == *version => Ok(()),
        Some(v) => Err(format!("the release's TOC says {v}, its manifest {version}")),
        None => Err(format!("the release's TOC has no version, its manifest says {version}")),
    }
}

fn save(state: &AddonState, path: &Path, logger: &Logger) {
    if let Err(e) = state.save_to(path) {
        logger.error(&format!("addon: could not save its state: {e}"));
    }
}

/// Hands one `Foreign` AddOns folder over to the Companion.
pub fn mark_taken_over(state_path: &Path, dir: &str) -> Result<(), String> {
    let mut state = AddonState::load_from(state_path);
    match state.lines.get(dir) {
        Some(l) if l.kind == LineKind::Foreign && l.installed_version.is_some() => {}
        Some(l) if l.kind == LineKind::Foreign => {
            return Err("that folder is a linked or development copy, the Companion leaves it alone".to_string())
        }
        _ => return Err("that install is not kept by another app".to_string()),
    }
    // The card turns to "taken over" at once, before the next pass confirms it.
    if let Some(l) = state.lines.get_mut(dir) {
        l.kind = LineKind::TakenOver;
    }
    state.taken_over.insert(dir.to_string());
    state.save_to(state_path).map_err(|e| e.to_string())
}

fn config_dir(app: &AppHandle) -> Option<PathBuf> {
    app.state::<crate::state::AppState>().config_path.parent().map(Path::to_path_buf)
}

fn current_config(app: &AppHandle) -> Config {
    app.state::<crate::state::AppState>()
        .config
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone()
}

async fn run_now(app: &AppHandle, client: &reqwest::Client) -> Vec<AddonLine> {
    let Some(dir) = config_dir(app) else { return Vec::new() };
    let state = app.state::<crate::state::AppState>();
    let logger = state.logger.clone();
    let control = app.state::<AddonControl>();
    let ctx = Ctx { client, procs: &SystemProcesses, control: &control, logger: &logger };
    run_once(&ctx, &current_config(app), &dir.join(STATE_FILE_NAME), crate::luafile::now_unix()).await
}

pub async fn run_loop(app: AppHandle, _logger: Arc<Logger>) {
    let client = crate::sync::build_client();
    tokio::time::sleep(STARTUP_DELAY).await;
    let mut failed_in_a_row = 0u32;
    loop {
        let lines = run_now(&app, &client).await;
        failed_in_a_row = if lines.iter().any(|l| l.kind == LineKind::Failed) { failed_in_a_row.saturating_add(1) } else { 0 };
        let wait = next_wait(&lines, failed_in_a_row);
        let control = app.state::<AddonControl>();
        tokio::select! {
            _ = tokio::time::sleep(wait) => {}
            _ = control.wake.notified() => {}
        }
    }
}

/// Backs the wizard's last step and Settings: one pass now.
#[tauri::command]
pub async fn addon_check_now(app: AppHandle) -> Vec<AddonLine> {
    run_now(&app, &crate::sync::build_client()).await
}

#[tauri::command]
pub async fn addon_take_over(app: AppHandle, dir: String) -> Result<(), String> {
    let path = config_dir(&app).ok_or("no config folder")?.join(STATE_FILE_NAME);
    let control = app.state::<AddonControl>();
    {
        // Not while a pass is writing the same file.
        let _serial = control.running.lock().await;
        mark_taken_over(&path, &dir)?;
    }
    control.wake.notify_one();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::addon_folder::write_marker;
    use crate::addon_game::GameProcesses;
    use crate::games::GameKind;
    use std::collections::HashMap;
    use std::fs;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    const RELEASE: &[u8] = include_bytes!("../tests/fixtures/addon/release.json");
    const RELEASE_SIG: &[u8] = include_bytes!("../tests/fixtures/addon/release.json.sig");
    const OTHER_SIG: &[u8] = include_bytes!("../tests/fixtures/addon/addon.json.sig");
    const ZIP: &[u8] = include_bytes!("../tests/fixtures/addon/GoldCap-addon-v0.23.0.zip");
    const TEST_PUB: &str = include_str!("../tests/fixtures/addon/test.pub");
    const ZIP_PATH: &str = "/GoldCap-addon-v0.23.0.zip";

    fn v(a: u32, b: u32, c: u32) -> Version {
        Version(a, b, c)
    }

    // A one-thread HTTP/1.1 responder that records what it was asked for.
    struct Server {
        base: String,
        hits: Arc<Mutex<Vec<String>>>,
        routes: Arc<Mutex<HashMap<String, Vec<u8>>>>,
        stop: Arc<AtomicBool>,
    }

    impl Server {
        fn start() -> Server {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let base = format!("http://{}/", listener.local_addr().unwrap());
            let hits = Arc::new(Mutex::new(Vec::new()));
            let routes: Arc<Mutex<HashMap<String, Vec<u8>>>> = Arc::new(Mutex::new(HashMap::from([
                ("/addon.json".to_string(), RELEASE.to_vec()),
                ("/addon.json.sig".to_string(), RELEASE_SIG.to_vec()),
                (ZIP_PATH.to_string(), ZIP.to_vec()),
            ])));
            let stop = Arc::new(AtomicBool::new(false));
            let (h, r, s) = (hits.clone(), routes.clone(), stop.clone());
            std::thread::spawn(move || {
                while !s.load(Ordering::Relaxed) {
                    let Ok((mut conn, _)) = listener.accept() else {
                        std::thread::sleep(std::time::Duration::from_millis(5));
                        continue;
                    };
                    conn.set_nonblocking(false).unwrap();
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 1024];
                    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                        match conn.read(&mut chunk) {
                            Ok(0) | Err(_) => break,
                            Ok(n) => buf.extend_from_slice(&chunk[..n]),
                        }
                    }
                    let text = String::from_utf8_lossy(&buf).to_string();
                    let path = text.split_whitespace().nth(1).unwrap_or("/").to_string();
                    h.lock().unwrap().push(path.clone());
                    let body = r.lock().unwrap().get(&path).cloned();
                    let head = match &body {
                        Some(b) => format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", b.len()),
                        None => "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string(),
                    };
                    let _ = conn.write_all(head.as_bytes());
                    if let Some(b) = body {
                        let _ = conn.write_all(&b);
                    }
                }
            });
            Server { base, hits, routes, stop }
        }

        fn set(&self, path: &str, body: &[u8]) {
            self.routes.lock().unwrap().insert(path.to_string(), body.to_vec());
        }

        fn zip_requests(&self) -> usize {
            self.hits.lock().unwrap().iter().filter(|p| p.ends_with(".zip")).count()
        }

        fn requests(&self) -> usize {
            self.hits.lock().unwrap().len()
        }
    }

    impl Drop for Server {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
        }
    }

    struct Procs(Vec<&'static str>);

    impl GameProcesses for Procs {
        fn executables(&self) -> std::io::Result<Vec<String>> {
            Ok(self.0.iter().map(|s| s.to_string()).collect())
        }
    }

    /// Lists no game on the first look; on every later look it first runs `then`
    /// and lists the game as running when `game_after` is set.
    struct Flips {
        looks: std::sync::atomic::AtomicUsize,
        then: Box<dyn Fn() + Send + Sync>,
        game_after: bool,
    }

    impl GameProcesses for Flips {
        fn executables(&self) -> std::io::Result<Vec<String>> {
            let mut out = vec!["explorer.exe".to_string()];
            if self.looks.fetch_add(1, Ordering::SeqCst) > 0 {
                (self.then)();
                if self.game_after {
                    out.push("Wow.exe".to_string());
                }
            }
            Ok(out)
        }
    }

    /// Lists no client by name, but an exe in `dir` is held open from the
    /// `from`th look on (0 is the first).
    struct HeldExe {
        dir: PathBuf,
        from: usize,
        looks: std::sync::atomic::AtomicUsize,
    }

    impl GameProcesses for HeldExe {
        fn executables(&self) -> std::io::Result<Vec<String>> {
            Ok(vec!["explorer.exe".to_string()])
        }

        fn exe_in_use(&self, game_dir: &Path) -> Option<String> {
            let look = self.looks.fetch_add(1, Ordering::SeqCst);
            (game_dir == self.dir && look >= self.from).then(|| "WowNewClient.exe is in use".to_string())
        }
    }

    struct Rig {
        _tmp: tempfile::TempDir,
        server: Server,
        config: Config,
        retail: PathBuf,
        state_path: PathBuf,
        control: AddonControl,
        logger: Logger,
    }

    impl Rig {
        fn new() -> Rig {
            let tmp = tempfile::tempdir().unwrap();
            let retail = tmp.path().join("wow/_retail_");
            fs::create_dir_all(&retail).unwrap();
            let config = Config {
                retail_enabled: true,
                wow_retail_path: retail.to_string_lossy().to_string(),
                ..Config::default()
            };
            let state_dir = tmp.path().join("config");
            Rig {
                logger: Logger::new(&state_dir).unwrap(),
                state_path: state_dir.join(STATE_FILE_NAME),
                _tmp: tmp,
                server: Server::start(),
                config,
                retail,
                control: AddonControl::default(),
            }
        }

        fn addons(&self) -> PathBuf {
            self.retail.join("Interface/AddOns")
        }

        fn logger_file(&self) -> PathBuf {
            self.state_path.parent().unwrap().join(crate::logging::LOG_FILE_NAME)
        }

        fn key(&self) -> String {
            self.addons().to_string_lossy().to_string()
        }

        /// A `GoldCap` folder as an installer leaves it.
        fn hand_made(&self, version: &str, marker: bool) {
            let d = self.addons().join("GoldCap");
            fs::create_dir_all(d.join("Core")).unwrap();
            fs::write(d.join("GoldCap.toc"), format!("## Version: addon-v{version}\n")).unwrap();
            fs::write(d.join("Core/old.lua"), "old").unwrap();
            if marker {
                write_marker(&d, &Version::parse(version).unwrap(), 1).unwrap();
            }
        }

        fn take_over(&self) {
            let mut s = AddonState::default();
            s.taken_over.insert(self.key());
            s.save_to(&self.state_path).unwrap();
        }

        async fn run(&self, procs: &[&'static str]) -> Vec<AddonLine> {
            // A real machine always lists some processes; an empty list means "unknown".
            let mut all = vec!["explorer.exe"];
            all.extend_from_slice(procs);
            self.run_with(&Procs(all)).await
        }

        async fn run_with(&self, procs: &dyn GameProcesses) -> Vec<AddonLine> {
            let ctx = Ctx { client: &crate::sync::build_client(), procs, control: &self.control, logger: &self.logger };
            let src = Source { manifest_url: &format!("{}addon.json", self.server.base), url_prefix: URL_PREFIX, pubkey: TEST_PUB };
            run_once_with_key(&ctx, &src, &self.config, &self.state_path, 1_800_000_000).await
        }
    }

    fn tree(dir: &Path) -> Vec<(String, Vec<u8>)> {
        fn walk(base: &Path, dir: &Path, out: &mut Vec<(String, Vec<u8>)>) {
            let mut es: Vec<_> = fs::read_dir(dir).unwrap().map(|e| e.unwrap().path()).collect();
            es.sort();
            for p in es {
                let rel = p.strip_prefix(base).unwrap().to_string_lossy().to_string();
                if p.is_dir() {
                    out.push((format!("{rel}/"), Vec::new()));
                    walk(base, &p, out);
                } else {
                    out.push((rel, fs::read(&p).unwrap()));
                }
            }
        }
        let mut out = Vec::new();
        walk(dir, dir, &mut out);
        out
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut n: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        n.sort();
        n
    }

    // The planning table.

    #[test]
    fn plan_follows_the_table() {
        use Action::{Apply, Skip};
        use FolderState::{Foreign, Missing, Ours};
        let a = v(0, 23, 0);
        assert_eq!(plan(&Missing, false, &a, false), Apply(Decision::Install));
        assert_eq!(plan(&Missing, false, &a, true), Skip(LineKind::Waiting));
        assert_eq!(plan(&Ours(v(0, 22, 0)), false, &a, false), Apply(Decision::Update));
        assert_eq!(plan(&Ours(v(0, 22, 0)), false, &a, true), Skip(LineKind::Waiting));
        assert_eq!(plan(&Ours(v(0, 23, 0)), false, &a, false), Skip(LineKind::Managed));
        assert_eq!(plan(&Ours(v(0, 24, 0)), false, &a, false), Skip(LineKind::Managed));
        assert_eq!(plan(&Foreign(Some(v(0, 22, 0))), false, &a, false), Skip(LineKind::Foreign));
        assert_eq!(plan(&Foreign(Some(v(0, 22, 0))), true, &a, false), Apply(Decision::Update));
        assert_eq!(plan(&Foreign(Some(v(0, 23, 0))), true, &a, false), Skip(LineKind::TakenOver));
        assert_eq!(plan(&Foreign(None), true, &a, false), Skip(LineKind::Foreign));
    }

    fn line_of(kind: LineKind) -> AddonLine {
        AddonLine {
            dir: "/AddOns".into(),
            folder: "_retail_".into(),
            game: GameKind::Retail,
            kind,
            installed_version: None,
            available_version: None,
            updated_at: None,
            last_change: None,
            error: None,
        }
    }

    #[test]
    fn waits_two_minutes_only_while_something_waits() {
        assert_eq!(next_wait(&[line_of(LineKind::Waiting)], 0), std::time::Duration::from_secs(120));
        assert_eq!(next_wait(&[line_of(LineKind::Managed)], 0), crate::updater::CHECK_INTERVAL);
        assert_eq!(next_wait(&[], 0), crate::updater::CHECK_INTERVAL);
    }

    #[test]
    fn a_failure_is_tried_again_in_two_minutes_then_less_often() {
        let failed = [line_of(LineKind::Managed), line_of(LineKind::Failed)];
        let min = |m: u64| std::time::Duration::from_secs(m * 60);
        assert_eq!(next_wait(&failed, 1), min(2));
        assert_eq!(next_wait(&failed, 2), min(4));
        assert_eq!(next_wait(&failed, 3), min(8));
        assert_eq!(next_wait(&failed, 8), crate::updater::CHECK_INTERVAL);
        assert_eq!(next_wait(&failed, u32::MAX), crate::updater::CHECK_INTERVAL);
        // Something waiting for the game keeps the two-minute look.
        assert_eq!(next_wait(&[line_of(LineKind::Failed), line_of(LineKind::Waiting)], 6), min(2));
    }

    fn release_0_23() -> VerifiedRelease {
        VerifiedRelease { manifest: serde_json::from_slice(RELEASE).unwrap(), version: v(0, 23, 0), zip: ZIP.to_vec() }
    }

    #[test]
    fn a_swap_whose_rollback_also_fails_puts_the_old_folder_back_at_once() {
        use crate::addon_files::SwapStep;
        let rig = Rig::new();
        rig.hand_made("0.22.0", true);
        let before = tree(&rig.addons().join("GoldCap"));
        let t = &targets(&rig.config)[0];
        let r = apply_steps(t, &release_0_23(), 1_800_000_000, &|s| match s {
            SwapStep::MoveNewIn | SwapStep::PutOldBack => Err(std::io::Error::other("held by a scanner")),
            _ => Ok(()),
        });
        assert!(r.unwrap_err().contains("held by a scanner"));
        assert_eq!(tree(&rig.addons().join("GoldCap")), before);
        assert_eq!(names(&rig.addons()), vec!["GoldCap"]);
    }

    #[test]
    fn a_release_whose_toc_disagrees_with_its_manifest_is_refused() {
        // The fixture zip's TOC says 0.23.0; this manifest says 0.24.0.
        let mut release = release_0_23();
        release.version = v(0, 24, 0);
        let rig = Rig::new();
        rig.hand_made("0.22.0", true);
        let before = tree(&rig.addons());
        let t = &targets(&rig.config)[0];
        let e = apply(t, &release, 1_800_000_000).unwrap_err();
        assert!(e.contains("0.23.0") && e.contains("0.24.0"), "{e}");
        assert_eq!(tree(&rig.addons()), before);
        // A first install leaves nothing behind either.
        let rig = Rig::new();
        assert!(apply(&targets(&rig.config)[0], &release, 1_800_000_000).is_err());
        assert!(names(&rig.addons()).is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_failed_swap_is_tried_again_with_the_download_it_kept() {
        use std::os::unix::fs::PermissionsExt;
        let rig = Rig::new();
        rig.hand_made("0.22.0", true);
        fs::set_permissions(rig.addons(), fs::Permissions::from_mode(0o555)).unwrap();
        let lines = rig.run(&[]).await;
        fs::set_permissions(rig.addons(), fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(lines[0].kind, LineKind::Failed);
        let lines = rig.run(&[]).await;
        assert_eq!(lines[0].kind, LineKind::Managed);
        assert_eq!(rig.server.zip_requests(), 1, "the verified download is reused");
    }

    // Targets.

    #[test]
    fn targets_lists_retail_and_forever_only_when_enabled() {
        let tmp = tempfile::tempdir().unwrap();
        let retail = tmp.path().join("_retail_");
        let root = tmp.path().join("root");
        fs::create_dir_all(&retail).unwrap();
        fs::create_dir_all(root.join("_classic_beta_")).unwrap();
        fs::create_dir_all(root.join("_classic_era_")).unwrap();
        let mut config = Config {
            retail_enabled: true,
            forever_enabled: true,
            wow_retail_path: retail.to_string_lossy().to_string(),
            forever_root_path: root.to_string_lossy().to_string(),
            ..Config::default()
        };
        let t = targets(&config);
        assert_eq!(t.len(), 2);
        assert_eq!((t[0].game, t[0].folder.as_str()), (GameKind::Retail, "_retail_"));
        assert_eq!((t[1].game, t[1].folder.as_str()), (GameKind::Forever, "_classic_beta_"));
        assert_eq!(t[1].addons_dir(), root.join("_classic_beta_/Interface/AddOns"));
        config.forever_enabled = false;
        assert_eq!(targets(&config).len(), 1);
        config.retail_enabled = false;
        assert!(targets(&config).is_empty());
        config.retail_enabled = true;
        config.wow_retail_path = tmp.path().join("gone").to_string_lossy().to_string();
        assert!(targets(&config).is_empty());
    }

    #[test]
    fn an_unplayed_forever_beta_folder_is_a_target() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("_classic_beta_")).unwrap();
        let config = Config {
            forever_enabled: true,
            forever_root_path: tmp.path().to_string_lossy().to_string(),
            ..Config::default()
        };
        assert_eq!(targets(&config).len(), 1);
    }

    // run_once, end to end against the local server.

    #[tokio::test]
    async fn installs_into_an_empty_addons_folder_and_writes_the_marker() {
        let rig = Rig::new();
        let lines = rig.run(&[]).await;
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].kind, LineKind::Managed);
        assert_eq!(lines[0].last_change, Some(Change::Installed));
        assert_eq!(lines[0].installed_version.as_deref(), Some("0.23.0"));
        assert_eq!(lines[0].game, GameKind::Retail);
        assert!(rig.addons().join("GoldCap/Core/a.lua").is_file());
        assert!(rig.addons().join("GoldCap/.goldcap-companion.json").is_file());
        assert_eq!(names(&rig.addons()), vec!["GoldCap"]);
        assert_eq!(AddonState::load_from(&rig.state_path).lines.len(), 1);
    }

    #[tokio::test]
    async fn updates_a_folder_it_installed() {
        let rig = Rig::new();
        rig.hand_made("0.22.0", true);
        let lines = rig.run(&[]).await;
        assert_eq!(lines[0].kind, LineKind::Managed);
        assert_eq!(lines[0].last_change, Some(Change::Updated));
        assert!(!rig.addons().join("GoldCap/Core/old.lua").exists());
        assert!(rig.addons().join("GoldCap/Core/a.lua").exists());
    }

    #[tokio::test]
    async fn leaves_a_curseforge_copy_alone() {
        let rig = Rig::new();
        rig.hand_made("0.20.0", false);
        let before = tree(&rig.addons());
        let lines = rig.run(&[]).await;
        assert_eq!(lines[0].kind, LineKind::Foreign);
        assert_eq!(lines[0].installed_version.as_deref(), Some("0.20.0"));
        assert_eq!(tree(&rig.addons()), before);
        assert_eq!(rig.server.zip_requests(), 0);
    }

    #[tokio::test]
    async fn take_over_updates_it_and_writes_the_marker() {
        let rig = Rig::new();
        rig.hand_made("0.20.0", false);
        rig.take_over();
        let lines = rig.run(&[]).await;
        assert_eq!(lines[0].kind, LineKind::Managed);
        assert!(rig.addons().join("GoldCap/.goldcap-companion.json").is_file());
        assert!(rig.addons().join("GoldCap/Core/a.lua").exists());
        assert!(AddonState::load_from(&rig.state_path).taken_over.is_empty());
    }

    #[tokio::test]
    async fn does_nothing_while_the_game_runs_and_applies_when_it_closes() {
        let rig = Rig::new();
        let lines = rig.run(&["Wow.exe"]).await;
        assert_eq!(lines[0].kind, LineKind::Waiting);
        assert_eq!(lines[0].available_version.as_deref(), Some("0.23.0"));
        assert!(!rig.addons().join("GoldCap").exists());
        assert_eq!(rig.server.zip_requests(), 1);
        let lines = rig.run(&[]).await;
        assert_eq!(lines[0].kind, LineKind::Managed);
        assert!(rig.addons().join("GoldCap/Core/a.lua").exists());
        assert_eq!(rig.server.zip_requests(), 1, "the kept download is reused");
    }

    struct NoListing;

    impl GameProcesses for NoListing {
        fn executables(&self) -> std::io::Result<Vec<String>> {
            Err(std::io::Error::other("tasklist is blocked"))
        }
    }

    #[tokio::test]
    async fn a_process_list_that_cannot_be_read_waits_and_says_why() {
        let rig = Rig::new();
        rig.hand_made("0.22.0", true);
        let before = tree(&rig.addons());
        let lines = rig.run_with(&NoListing).await;
        assert_eq!(lines[0].kind, LineKind::Waiting);
        assert!(lines[0].error.as_deref().is_some_and(|e| e.contains("tasklist is blocked")), "{:?}", lines[0].error);
        assert_eq!(tree(&rig.addons()), before);
        let log = fs::read_to_string(rig.logger_file()).unwrap();
        assert!(log.contains("tasklist is blocked"), "{log}");
        // A game that is simply running carries no such reason.
        let lines = rig.run(&["Wow.exe"]).await;
        assert_eq!((lines[0].kind, lines[0].error.as_deref()), (LineKind::Waiting, None));
    }

    #[tokio::test]
    async fn a_game_that_starts_during_the_download_stops_the_swap_and_keeps_the_download() {
        let rig = Rig::new();
        rig.hand_made("0.22.0", true);
        let before = tree(&rig.addons().join("GoldCap"));
        let flips = Flips { looks: Default::default(), then: Box::new(|| {}), game_after: true };
        let lines = rig.run_with(&flips).await;
        assert_eq!(lines[0].kind, LineKind::Waiting);
        assert_eq!(tree(&rig.addons().join("GoldCap")), before);
        assert!(!names(&rig.addons()).iter().any(|n| n.contains(".old-") || n.starts_with(".goldcap-staging")));
        let log = fs::read_to_string(rig.logger_file()).unwrap();
        assert!(log.contains("changed while the update was downloading"), "{log}");
        let lines = rig.run(&[]).await;
        assert_eq!(lines[0].kind, LineKind::Managed);
        assert_eq!(rig.server.zip_requests(), 1, "the staged download is kept");
    }

    #[tokio::test]
    async fn an_exe_held_in_the_game_folder_counts_as_the_game_running() {
        let rig = Rig::new();
        rig.hand_made("0.22.0", true);
        let before = tree(&rig.addons());
        let held = HeldExe { dir: rig.retail.clone(), from: 0, looks: Default::default() };
        let lines = rig.run_with(&held).await;
        assert_eq!(lines[0].kind, LineKind::Waiting);
        assert_eq!(tree(&rig.addons()), before);
        let log = fs::read_to_string(rig.logger_file()).unwrap();
        assert!(log.contains("WowNewClient.exe is in use"), "{log}");
        let lines = rig.run(&[]).await;
        assert_eq!(lines[0].kind, LineKind::Managed);
    }

    #[tokio::test]
    async fn an_exe_held_once_the_download_is_done_stops_the_swap() {
        let rig = Rig::new();
        rig.hand_made("0.22.0", true);
        let before = tree(&rig.addons());
        let held = HeldExe { dir: rig.retail.clone(), from: 1, looks: Default::default() };
        let lines = rig.run_with(&held).await;
        assert_eq!(lines[0].kind, LineKind::Waiting);
        assert_eq!(tree(&rig.addons()), before);
    }

    #[tokio::test]
    async fn a_folder_that_changes_during_the_download_is_not_swapped() {
        let rig = Rig::new();
        let addons = rig.addons();
        // Another installer puts its own copy in while the zip comes down.
        let then: Box<dyn Fn() + Send + Sync> = Box::new(move || {
            let d = addons.join("GoldCap");
            fs::create_dir_all(&d).unwrap();
            fs::write(d.join("GoldCap.toc"), "## Version: addon-v0.21.0\n").unwrap();
            fs::write(d.join("theirs.lua"), "theirs").unwrap();
        });
        let flips = Flips { looks: Default::default(), then, game_after: false };
        let lines = rig.run_with(&flips).await;
        assert_eq!(lines[0].kind, LineKind::Waiting);
        assert_eq!(fs::read(rig.addons().join("GoldCap/theirs.lua")).unwrap(), b"theirs");
        assert!(!rig.addons().join("GoldCap/Core/a.lua").exists());
        let log = fs::read_to_string(rig.logger_file()).unwrap();
        assert!(log.contains("changed while the update was downloading"), "{log}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn two_installs_sharing_one_addons_folder_are_both_kept_after_one_pass() {
        let mut rig = Rig::new();
        let root = rig.retail.parent().unwrap().to_path_buf();
        let beta = root.join("_classic_beta_/Interface");
        fs::create_dir_all(&beta).unwrap();
        fs::create_dir_all(rig.addons()).unwrap();
        std::os::unix::fs::symlink(rig.addons(), beta.join("AddOns")).unwrap();
        rig.config.forever_enabled = true;
        rig.config.forever_root_path = root.to_string_lossy().to_string();
        let lines = rig.run(&[]).await;
        assert_eq!(lines.len(), 2);
        for l in &lines {
            assert_eq!((l.kind, l.last_change), (LineKind::Managed, Some(Change::Installed)), "{}", l.folder);
            assert_eq!(l.installed_version.as_deref(), Some("0.23.0"), "{}", l.folder);
        }
        assert_eq!(names(&rig.addons()), vec!["GoldCap"]);
        let log = fs::read_to_string(rig.logger_file()).unwrap();
        assert!(!log.contains("changed while the update was downloading"), "{log}");
    }

    #[tokio::test]
    async fn a_tampered_zip_changes_nothing() {
        let rig = Rig::new();
        rig.hand_made("0.22.0", true);
        let before = tree(&rig.addons());
        let mut bad = ZIP.to_vec();
        let last = bad.len() / 2;
        bad[last] ^= 0xff;
        rig.server.set(ZIP_PATH, &bad);
        let lines = rig.run(&[]).await;
        assert_eq!(lines[0].kind, LineKind::Failed);
        assert!(lines[0].error.is_some());
        assert_eq!(tree(&rig.addons()), before);
        assert_eq!(names(&rig.addons()), vec!["GoldCap"]);
    }

    #[tokio::test]
    async fn a_missing_zip_on_a_first_install_shows_failed_with_the_reason() {
        let rig = Rig::new();
        rig.server.set(ZIP_PATH, b"");
        let lines = rig.run(&[]).await;
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].kind, LineKind::Failed);
        assert!(lines[0].error.as_deref().is_some_and(|e| !e.is_empty()));
    }

    #[tokio::test]
    async fn a_bad_signature_changes_nothing() {
        let rig = Rig::new();
        rig.hand_made("0.22.0", true);
        let before = tree(&rig.addons());
        rig.server.set("/addon.json.sig", OTHER_SIG);
        rig.run(&[]).await;
        assert_eq!(tree(&rig.addons()), before);
        assert_eq!(rig.server.zip_requests(), 0);
    }

    #[tokio::test]
    async fn never_downgrades_when_a_manager_is_ahead() {
        let rig = Rig::new();
        rig.hand_made("0.24.0", true);
        let before = tree(&rig.addons());
        let lines = rig.run(&[]).await;
        assert_eq!(lines[0].kind, LineKind::Managed);
        assert_eq!(lines[0].installed_version.as_deref(), Some("0.24.0"));
        assert_eq!(tree(&rig.addons()), before);
        assert_eq!(rig.server.zip_requests(), 0);
    }

    #[tokio::test]
    async fn off_means_no_network_at_all() {
        let mut rig = Rig::new();
        rig.config.addon_install = false;
        let lines = rig.run(&[]).await;
        assert_eq!(lines[0].kind, LineKind::Off);
        assert_eq!(rig.server.requests(), 0);
        assert!(!rig.addons().exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_failed_swap_reports_failed_and_keeps_the_old_folder() {
        use std::os::unix::fs::PermissionsExt;
        let rig = Rig::new();
        rig.hand_made("0.22.0", true);
        let before = tree(&rig.addons());
        fs::set_permissions(rig.addons(), fs::Permissions::from_mode(0o555)).unwrap();
        let lines = rig.run(&[]).await;
        fs::set_permissions(rig.addons(), fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(lines[0].kind, LineKind::Failed);
        assert!(lines[0].error.is_some());
        assert_eq!(tree(&rig.addons()), before);
        assert_eq!(names(&rig.addons()), vec!["GoldCap"]);
    }

    #[tokio::test]
    async fn state_survives_a_corrupt_file() {
        let rig = Rig::new();
        fs::create_dir_all(rig.state_path.parent().unwrap()).unwrap();
        fs::write(&rig.state_path, "{nope").unwrap();
        let lines = rig.run(&[]).await;
        assert_eq!(lines[0].kind, LineKind::Managed);
    }

    #[tokio::test]
    async fn a_crash_leftover_is_restored_before_anything_else() {
        let rig = Rig::new();
        rig.hand_made("0.23.0", true);
        fs::rename(rig.addons().join("GoldCap"), crate::addon_files::aside_dir(&rig.addons(), 1_799_000_000)).unwrap();
        let lines = rig.run(&[]).await;
        assert_eq!(lines[0].kind, LineKind::Managed);
        assert_eq!(names(&rig.addons()), vec!["GoldCap"]);
        assert_eq!(rig.server.zip_requests(), 0);
    }

    #[tokio::test]
    async fn only_foreign_lines_can_be_taken_over() {
        let rig = Rig::new();
        rig.hand_made("0.20.0", false);
        rig.run(&[]).await;
        let key = rig.key();
        assert!(mark_taken_over(&rig.state_path, &key).is_ok());
        assert!(AddonState::load_from(&rig.state_path).taken_over.contains(&key));
        assert!(mark_taken_over(&rig.state_path, "/somewhere/else").is_err());
    }

    #[tokio::test]
    async fn taking_over_turns_the_saved_card_to_taken_over_at_once() {
        let rig = Rig::new();
        rig.hand_made("0.20.0", false);
        rig.run(&[]).await;
        mark_taken_over(&rig.state_path, &rig.key()).unwrap();
        let line = AddonState::load_from(&rig.state_path).lines[&rig.key()].clone();
        assert_eq!(line.kind, LineKind::TakenOver);
        assert_eq!(line.installed_version.as_deref(), Some("0.20.0"));
    }

    #[tokio::test]
    async fn a_folder_with_no_readable_version_cannot_be_taken_over() {
        let rig = Rig::new();
        fs::create_dir_all(rig.addons().join("GoldCap")).unwrap();
        let lines = rig.run(&[]).await;
        assert_eq!(lines[0].kind, LineKind::Foreign);
        assert!(lines[0].installed_version.is_none());
        assert!(mark_taken_over(&rig.state_path, &rig.key()).is_err());
        assert!(AddonState::load_from(&rig.state_path).taken_over.is_empty());
    }
}
