//! The sync loop: fetch the per-realm GCS1 import string from the public
//! GoldCap API on an interval (plus an on-demand "sync now" trigger), and
//! write it into the WoW addon folder. Errors are logged and reflected in
//! `SyncStatus` — the loop itself never stops on a failed tick.

use crate::config::Config;
use crate::logging::Logger;
use crate::luafile;
use std::fmt;
use std::path::Path;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;
use tokio::sync::{mpsc, watch};

const IMPORT_STRING_URL: &str = "https://api.goldcap.gg/v1/addon/import-string";

#[derive(Debug)]
pub enum SyncError {
    Request(String),
    BadStatus(u16),
    InvalidBody,
    Write(String),
}

impl fmt::Display for SyncError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SyncError::Request(msg) => write!(f, "request failed: {msg}"),
            SyncError::BadStatus(code) => write!(f, "unexpected status {code}"),
            SyncError::InvalidBody => write!(f, "response was not a GCS1 import string"),
            SyncError::Write(msg) => write!(f, "failed to write addon files: {msg}"),
        }
    }
}

/// Which leg of the pipeline a `SyncError` belongs to — the Status screen
/// attributes a broken tick to exactly one of its three stage rows, and a
/// write failure and a fetch failure are different rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncErrorStage {
    /// The problem was reaching goldcap.gg: the request itself, its status
    /// code, or a body that was not a GCS1 import string.
    Prices,
    /// The fetch succeeded; writing the result into the addon folder failed.
    Addon,
}

impl SyncError {
    pub fn stage(&self) -> SyncErrorStage {
        match self {
            SyncError::Write(_) => SyncErrorStage::Addon,
            SyncError::Request(_) | SyncError::BadStatus(_) | SyncError::InvalidBody => {
                SyncErrorStage::Prices
            }
        }
    }
}

/// Shared, tray-readable snapshot of the last sync attempt.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct SyncStatus {
    pub last_success_at: Option<SystemTime>,
    pub last_success_realm: Option<String>,
    pub last_error: Option<String>,
    /// Which stage `last_error` belongs to. `None` whenever `last_error` is
    /// — the two are only ever set together, by the same tick.
    pub last_error_stage: Option<SyncErrorStage>,
    /// When a tick last ran, successful or not. Distinct from
    /// `last_success_at`: a run of failures must not look like silence.
    pub last_attempt_at: Option<SystemTime>,
    /// A tick is in flight. Drives the pulsing "Syncing…" state in the UI.
    pub syncing: bool,
    /// When the interval timer will next fire. Republished whenever the
    /// interval changes so the countdown never reflects a stale setting.
    pub next_tick_at: Option<SystemTime>,
    pub upload: crate::upload::UploadStats,
}

impl SyncStatus {
    /// Tray label: `"synced dentarg 12m ago"` / `"error: <short>"` /
    /// `"not synced yet"` before the first attempt completes.
    pub fn label(&self) -> String {
        if let Some(err) = &self.last_error {
            return format!("error: {}", truncate(err, 60));
        }
        match (&self.last_success_realm, self.last_success_at) {
            (Some(realm), Some(at)) => format!("synced {realm} {}", humanize_age(at)),
            _ => "not synced yet".to_string(),
        }
    }
}

/// Publishes the next scheduled tick. Free function rather than a method so
/// the loop can call it while holding nothing else.
pub fn schedule_next_tick(status: &Arc<Mutex<SyncStatus>>, at: SystemTime) {
    let mut s = status.lock().unwrap_or_else(|p| p.into_inner());
    s.next_tick_at = Some(at);
}

fn set_syncing(status: &Arc<Mutex<SyncStatus>>, syncing: bool) {
    let mut s = status.lock().unwrap_or_else(|p| p.into_inner());
    s.syncing = syncing;
    if syncing {
        s.last_attempt_at = Some(SystemTime::now());
    }
}

fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max_chars.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn humanize_age(at: SystemTime) -> String {
    let elapsed = SystemTime::now()
        .duration_since(at)
        .unwrap_or_default()
        .as_secs();
    if elapsed < 60 {
        "just now".to_string()
    } else if elapsed < 3600 {
        format!("{}m ago", elapsed / 60)
    } else if elapsed < 86_400 {
        format!("{}h ago", elapsed / 3600)
    } else {
        format!("{}d ago", elapsed / 86_400)
    }
}

pub fn build_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .user_agent(concat!("goldcap-companion/", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("reqwest client with only timeout/user-agent set should always build")
}

/// Fetches the import string for `region`/`realm`. Only a 2xx response
/// whose body starts with the GCS1 magic prefix counts as success — a
/// non-200 (including the API's own `realm_not_found`/`no_data` 404 bodies)
/// or an unexpected body is a `SyncError`, never written to disk.
pub async fn fetch_import_string(
    client: &reqwest::Client,
    region: &str,
    realm: &str,
) -> Result<String, SyncError> {
    let resp = client
        .get(IMPORT_STRING_URL)
        .query(&[("region", region), ("realm", realm)])
        .send()
        .await
        .map_err(|e| SyncError::Request(e.to_string()))?;

    let status = resp.status();
    if !status.is_success() {
        return Err(SyncError::BadStatus(status.as_u16()));
    }

    let body = resp
        .text()
        .await
        .map_err(|e| SyncError::Request(e.to_string()))?;
    if !luafile::is_valid_gcs1_body(&body) {
        return Err(SyncError::InvalidBody);
    }
    Ok(body)
}

/// Writes a freshly fetched import string into `{wow_retail_path}/Interface/AddOns/GoldCap_AppData/`.
pub fn apply_import_string(wow_retail_path: &Path, import_string: &str) -> Result<(), SyncError> {
    let dir = luafile::addon_dir(wow_retail_path);
    luafile::ensure_toc(&dir).map_err(|e| SyncError::Write(e.to_string()))?;
    luafile::write_app_data_lua(&dir, import_string, luafile::now_unix())
        .map_err(|e| SyncError::Write(e.to_string()))
}

/// Realm display name -> connected-realm slug, remembered for the process's
/// lifetime. Auto-follow re-reads the game's folders every tick, but the name
/// it finds almost never changes, and resolving it is a network call.
fn slug_cache() -> &'static Mutex<HashMap<(String, String), String>> {
    static CACHE: OnceLock<Mutex<HashMap<(String, String), String>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Resolve a realm display name (any locale, member or leader name) to the
/// connected-realm slug the API expects. Shared with the `resolve_realm`
/// command so the picker and the sync loop can never disagree.
pub async fn resolve_realm_slug(
    client: &reqwest::Client,
    region: &str,
    name: &str,
) -> Result<String, String> {
    let key = (region.to_string(), name.to_ascii_lowercase());
    if let Some(hit) = slug_cache()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&key)
        .cloned()
    {
        return Ok(hit);
    }

    #[derive(serde::Deserialize)]
    struct Resolved {
        slug: String,
    }
    let resp = client
        .get("https://api.goldcap.gg/v1/addon/resolve-realm")
        .query(&[("region", region), ("name", name)])
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let slug = match resp.status().as_u16() {
        200 => resp.json::<Resolved>().await.map_err(|e| e.to_string())?.slug,
        404 => return Err(format!("realm \"{name}\" not found on {region}")),
        code => return Err(format!("resolve failed: HTTP {code}")),
    };
    slug_cache()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert(key, slug.clone());
    Ok(slug)
}

/// Which realm this tick is for.
///
/// Pinned mode is the stored slug. Auto mode reads the realm folders WoW
/// keeps under `WTF/Account/` — already sorted newest-first by mtime, so the
/// first one is where the player logged in last — and resolves that name.
/// That is the answer for someone who plays a couple of hours on one realm
/// and a couple on another: the prices follow them instead of being pinned to
/// whichever realm they happened to configure once.
async fn effective_realm(
    client: &reqwest::Client,
    config: &Config,
    logger: &Logger,
) -> Option<String> {
    if !config.realm_auto {
        let pinned = config.realm_slug.trim();
        return (!pinned.is_empty()).then(|| pinned.to_string());
    }

    let names = crate::wtf::list_realm_names(Path::new(&config.wow_retail_path));
    let newest = names.first()?;
    match resolve_realm_slug(client, &config.region.to_string(), newest).await {
        Ok(slug) => Some(slug),
        Err(e) => {
            logger.error(&format!("could not resolve last played realm \"{newest}\": {e}"));
            // Fall back to whatever was last pinned rather than syncing
            // nothing at all.
            let pinned = config.realm_slug.trim();
            (!pinned.is_empty()).then(|| pinned.to_string())
        }
    }
}

/// Runs one sync attempt end to end: fetch, write, update `status`, log the
/// outcome. Never panics or propagates — a bad tick is just a logged error.
pub async fn sync_once(
    client: &reqwest::Client,
    config: &Config,
    status: &Arc<Mutex<SyncStatus>>,
    logger: &Logger,
    state_path: &Path,
) {
    set_syncing(status, true);

    let realm_slug = effective_realm(client, config, logger).await.unwrap_or_default();
    if realm_slug.is_empty() || config.wow_retail_path.trim().is_empty() {
        let msg = "not configured (realm or WoW path missing)".to_string();
        logger.error(&format!("sync skipped: {msg}"));
        if let Ok(mut s) = status.lock() {
            s.last_error = Some(msg);
            s.last_error_stage = Some(SyncErrorStage::Prices);
        }
        set_syncing(status, false);
        return;
    }

    let region = config.region.to_string();
    let fetch_result = fetch_import_string(client, &region, &realm_slug).await;

    // Fetching and writing are attributed separately from here on: a write
    // failure after a successful fetch must not make the prices stage look
    // broken (the site was reached fine), and must not erase the fact that
    // this tick's fetch really did just succeed.
    let (fetched_ok, sync_error) = match fetch_result {
        Ok(body) => match apply_import_string(Path::new(&config.wow_retail_path), &body) {
            Ok(()) => (true, None),
            Err(e) => (true, Some(e)),
        },
        Err(e) => (false, Some(e)),
    };

    // Ledger upload rides along on the same tick, but as a passenger: it runs
    // AFTER the price write and reports through the logger only, so a failed
    // upload can never turn a good price sync into a red tray label. An
    // unpaired companion (empty token) is a silent no-op.
    let upload_stats = crate::upload::upload_once(
        client,
        &config.companion_token,
        Path::new(&config.wow_retail_path),
        &state_path.join(crate::upload::STATE_FILE_NAME),
        logger,
    )
    .await;

    {
        let mut s = match status.lock() {
            Ok(s) => s,
            Err(poisoned) => poisoned.into_inner(),
        };
        s.upload = upload_stats;
        s.syncing = false;
        if fetched_ok {
            s.last_success_at = Some(SystemTime::now());
            s.last_success_realm = Some(realm_slug.clone());
        }
        match &sync_error {
            None => {
                s.last_error = None;
                s.last_error_stage = None;
                logger.info(&format!("synced {} {}", region, realm_slug));
            }
            Some(e) => {
                s.last_error_stage = Some(e.stage());
                s.last_error = Some(e.to_string());
                logger.error(&format!("sync failed for {} {}: {e}", region, realm_slug));
            }
        }
        // Lock released here (end of block), before the ledger summary
        // fetch below -- that fetch is the slowest leg of the tick (its own
        // 20s timeout) and status is done publishing by this point, so it
        // must not hold the tray on "Syncing..." a moment longer than the
        // price sync it actually reflects.
    }

    // The in-game Sold tab's data rides the same tick, also as a passenger,
    // deliberately after BOTH the upload leg (so the snapshot includes what
    // was just uploaded) AND status publication above (M7 ruling): this
    // fetch can take up to its own 20s timeout, and nothing about it
    // belongs in the tray's "syncing" window -- the price sync (what the
    // tray label is actually about) is already done and published by the
    // time this runs. Reporting is logger-only and SyncStatus stays
    // untouched from here on: the user-visible staleness surface is the
    // tab's own age line, and a failed fetch leaves the previous
    // LedgerSummary.lua on disk (apply_fetch_result writes nothing on Err).
    // Unpaired = silent no-op, same rule as the upload.
    if !config.companion_token.trim().is_empty() {
        let fetched =
            crate::ledger_summary::fetch_summary(client, &config.companion_token).await;
        let failed = fetched.as_ref().err().cloned();
        let dir = luafile::addon_dir(Path::new(&config.wow_retail_path));
        match crate::ledger_summary::apply_fetch_result(&dir, fetched, luafile::now_unix()) {
            Ok(true) => logger.info("ledger summary synced"),
            Ok(false) => {
                if let Some(e) = failed {
                    logger.error(&format!("ledger summary fetch failed: {e}"));
                }
            }
            Err(e) => logger.error(&format!("ledger summary write failed: {e}")),
        }
    }

    // Live observations ride the same passenger rule as the summary leg:
    // logger-only, never turns a good price sync red.
    crate::upload::upload_observations_once(
        client,
        &config.companion_token,
        &region,
        &realm_slug,
        Path::new(&config.wow_retail_path),
        &state_path.join(crate::upload::STATE_FILE_NAME),
        logger,
    )
    .await;
}

/// The sync loop: on every interval tick (recomputed from `config_rx`'s
/// current `intervalMinutes` whenever it changes) or "sync now" trigger,
/// runs one `sync_once`. `on_tick` is called after every attempt so the
/// caller (the tray) can refresh its label without this module depending on
/// tauri at all — keeping it plain, testable async/std code.
pub async fn run_loop(
    client: reqwest::Client,
    mut config_rx: watch::Receiver<Config>,
    mut trigger_rx: mpsc::Receiver<()>,
    status: Arc<Mutex<SyncStatus>>,
    logger: Arc<Logger>,
    state_dir: std::path::PathBuf,
    on_tick: impl Fn() + Send + 'static,
) {
    loop {
        let config = config_rx.borrow().clone();
        let interval = config.interval();
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    schedule_next_tick(&status, SystemTime::now() + interval);
                    sync_once(&client, &config, &status, &logger, &state_dir).await;
                    on_tick();
                }
                maybe = trigger_rx.recv() => {
                    if maybe.is_none() {
                        return; // sender dropped — app is shutting down
                    }
                    sync_once(&client, &config, &status, &logger, &state_dir).await;
                    on_tick();
                }
                changed = config_rx.changed() => {
                    if changed.is_err() {
                        return; // sender dropped — app is shutting down
                    }
                    break; // restart outer loop with the new config/interval
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_before_first_sync() {
        assert_eq!(SyncStatus::default().label(), "not synced yet");
    }

    #[test]
    fn a_write_failure_attributes_to_the_addon_stage() {
        assert_eq!(
            SyncError::Write("permission denied".into()).stage(),
            SyncErrorStage::Addon
        );
    }

    #[test]
    fn a_fetch_failure_attributes_to_the_prices_stage() {
        assert_eq!(
            SyncError::Request("timed out".into()).stage(),
            SyncErrorStage::Prices
        );
        assert_eq!(SyncError::BadStatus(500).stage(), SyncErrorStage::Prices);
        assert_eq!(SyncError::InvalidBody.stage(), SyncErrorStage::Prices);
    }

    #[test]
    fn label_reports_error_first() {
        let status = SyncStatus {
            last_success_at: Some(SystemTime::now()),
            last_success_realm: Some("dentarg".into()),
            last_error: Some("unexpected status 500".into()),
            ..SyncStatus::default()
        };
        assert_eq!(status.label(), "error: unexpected status 500");
    }

    #[test]
    fn label_reports_recent_success() {
        let status = SyncStatus {
            last_success_at: Some(SystemTime::now()),
            last_success_realm: Some("dentarg".into()),
            last_error: None,
            ..SyncStatus::default()
        };
        assert_eq!(status.label(), "synced dentarg just now");
    }

    #[test]
    fn label_reports_minutes_ago() {
        let status = SyncStatus {
            last_success_at: Some(SystemTime::now() - std::time::Duration::from_secs(12 * 60)),
            last_success_realm: Some("dentarg".into()),
            last_error: None,
            ..SyncStatus::default()
        };
        assert_eq!(status.label(), "synced dentarg 12m ago");
    }

    #[test]
    fn label_truncates_long_errors() {
        let status = SyncStatus {
            last_error: Some("x".repeat(200)),
            ..SyncStatus::default()
        };
        let label = status.label();
        assert!(label.starts_with("error: "));
        assert!(label.chars().count() <= "error: ".len() + 60);
        assert!(label.ends_with('…'));
    }

    #[test]
    fn a_fresh_status_is_idle_with_no_schedule() {
        let s = SyncStatus::default();
        assert!(!s.syncing);
        assert_eq!(s.next_tick_at, None);
        assert_eq!(s.last_attempt_at, None);
        assert_eq!(s.upload, crate::upload::UploadStats::default());
    }

    #[tokio::test]
    async fn an_unconfigured_attempt_still_clears_the_syncing_flag() {
        // The button must not be able to get stuck on "Syncing…" just because
        // the config is incomplete.
        let client = build_client();
        let config = Config::default();
        let status = Arc::new(Mutex::new(SyncStatus { syncing: true, ..SyncStatus::default() }));
        let dir = std::env::temp_dir()
            .join(format!("goldcap-companion-syncing-flag-{}", std::process::id()));
        let logger = Logger::new(&dir).unwrap();

        sync_once(&client, &config, &status, &logger, &dir).await;

        let s = status.lock().unwrap();
        assert!(!s.syncing);
        assert!(s.last_attempt_at.is_some(), "a refused attempt is still an attempt");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn scheduling_the_next_tick_publishes_it() {
        let status = Arc::new(Mutex::new(SyncStatus::default()));
        let at = SystemTime::now() + std::time::Duration::from_secs(1800);
        schedule_next_tick(&status, at);
        assert_eq!(status.lock().unwrap().next_tick_at, Some(at));
    }

    #[tokio::test]
    async fn sync_once_records_error_when_unconfigured() {
        let client = build_client();
        let config = Config::default(); // empty realm_slug + wow_retail_path
        let status = Arc::new(Mutex::new(SyncStatus::default()));
        let dir = std::env::temp_dir().join(format!(
            "goldcap-companion-sync-test-{}",
            std::process::id()
        ));
        let logger = Logger::new(&dir).unwrap();

        sync_once(&client, &config, &status, &logger, &dir).await;

        let s = status.lock().unwrap();
        assert!(s.last_error.is_some());
        assert!(s.last_success_at.is_none());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn apply_import_string_rejects_are_unreachable_for_invalid_bodies() {
        // apply_import_string itself trusts its caller to have already
        // validated the body (fetch_import_string is the gate) — this test
        // just documents that a valid body writes both files end to end.
        let dir = std::env::temp_dir().join(format!(
            "goldcap-companion-apply-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        apply_import_string(&dir, "GCS1;eu;dentarg;1;abc").unwrap();

        let addon_dir = dir.join("Interface").join("AddOns").join("GoldCap_AppData");
        assert!(addon_dir.join("GoldCap_AppData.toc").exists());
        assert!(addon_dir.join("AppData.lua").exists());

        std::fs::remove_dir_all(&dir).ok();
    }
}
