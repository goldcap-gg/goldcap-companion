use crate::addon_files::{recover, staging_dir, swap_in, unpack};
use crate::addon_folder::{decide, inspect, Decision, FolderState};
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

pub fn next_wait(has_waiting: bool) -> Duration {
    if has_waiting {
        WAITING_INTERVAL
    } else {
        crate::updater::CHECK_INTERVAL
    }
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
    let mut download_failed = false;
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
                    download_failed = true;
                }
            },
        }
    }
    let game_running = wanted && !download_failed && wow_is_running(ctx.procs);

    let mut lines = Vec::new();
    let mut applied = Vec::new();
    for (t, folder_state) in targets.iter().zip(&folders) {
        let prev = state.lines.get(&key(t)).cloned();
        let line = match plan(folder_state, taken(t), &available, game_running) {
            Action::Skip(kind) => {
                match kind {
                    LineKind::Foreign => ctx.logger.info(&format!(
                        "addon: {} GoldCap is kept by another app, leaving it",
                        t.folder
                    )),
                    LineKind::Waiting => ctx.logger.info(&format!(
                        "addon: {} update {available} waits for the game to close",
                        t.folder
                    )),
                    _ => {}
                }
                base_line(t, kind, folder_state, Some(&available), prev.as_ref())
            }
            Action::Apply(decision) => match (&release, download_failed) {
                (Some(r), false) => {
                    match apply(t, r, now) {
                        Ok(()) => {
                            let change = if decision == Decision::Install { Change::Installed } else { Change::Updated };
                            ctx.logger.info(&format!("addon: {} GoldCap {} is in place", t.folder, r.version));
                            applied.push(key(t));
                            let mut l = base_line(t, LineKind::Managed, folder_state, Some(&available), prev.as_ref());
                            l.installed_version = Some(r.version.to_string());
                            l.updated_at = Some(now);
                            l.last_change = Some(change);
                            l
                        }
                        Err(e) => {
                            ctx.logger.error(&format!("addon: {} GoldCap not changed: {e}", t.folder));
                            let mut l = base_line(t, LineKind::Failed, folder_state, Some(&available), prev.as_ref());
                            l.error = Some(e);
                            l
                        }
                    }
                }
                // The download was refused; the log says why and the line stays as it was.
                _ => match prev {
                    Some(p) => p,
                    None => continue,
                },
            },
        };
        lines.push(line);
    }

    for dir in applied {
        state.taken_over.remove(&dir);
    }
    if !lines.iter().any(|l| l.kind == LineKind::Waiting) {
        *ctx.control.pending.lock().unwrap_or_else(|p| p.into_inner()) = None;
    }
    state.lines = lines.iter().map(|l| (l.dir.clone(), l.clone())).collect();
    save(&state, state_path, ctx.logger);
    lines
}

/// Unpack beside `AddOns` and swap in; the staging folder never outlives the call.
fn apply(t: &Target, release: &VerifiedRelease, now: i64) -> Result<(), String> {
    let addons = t.addons_dir();
    std::fs::create_dir_all(&addons).map_err(|e| format!("could not create {}: {e}", addons.display()))?;
    let staging = staging_dir(&addons, now);
    let result = std::fs::create_dir_all(&staging)
        .map_err(|e| format!("could not create a staging folder: {e}"))
        .and_then(|_| unpack(&release.zip, &staging))
        .and_then(|_| swap_in(&addons, &staging.join("GoldCap"), &release.version, now));
    let _ = std::fs::remove_dir_all(&staging);
    result
}

fn save(state: &AddonState, path: &Path, logger: &Logger) {
    if let Err(e) = state.save_to(path) {
        logger.error(&format!("addon: could not save its state: {e}"));
    }
}

/// Hands one `Foreign` AddOns folder over to the Companion.
pub fn mark_taken_over(state_path: &Path, dir: &str) -> Result<(), String> {
    let mut state = AddonState::load_from(state_path);
    if state.lines.get(dir).map(|l| l.kind) != Some(LineKind::Foreign) {
        return Err("that install is not kept by another app".to_string());
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
    loop {
        let lines = run_now(&app, &client).await;
        let wait = next_wait(lines.iter().any(|l| l.kind == LineKind::Waiting));
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
            let procs = Procs(procs.to_vec());
            let ctx = Ctx { client: &crate::sync::build_client(), procs: &procs, control: &self.control, logger: &self.logger };
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

    #[test]
    fn waits_two_minutes_only_while_something_waits() {
        assert_eq!(next_wait(true), std::time::Duration::from_secs(120));
        assert_eq!(next_wait(false), crate::updater::CHECK_INTERVAL);
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
        assert!(lines.iter().all(|l| l.kind != LineKind::Failed));
        assert_eq!(tree(&rig.addons()), before);
        assert_eq!(names(&rig.addons()), vec!["GoldCap"]);
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
        if lines[0].kind == LineKind::Failed {
            assert!(lines[0].error.is_some());
        }
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
        fs::rename(rig.addons().join("GoldCap"), rig.addons().join("GoldCap.old-5")).unwrap();
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
}
