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
use std::time::{Duration, SystemTime};
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
    /// The whole-market payload written with the last successful tick: how many items
    /// it carried and its snapshot time. `None` when none was written.
    pub region: Option<crate::region::RegionSummary>,
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

/// Total time the shared client gives a request that does not set its own limit.
pub(crate) const CLIENT_TIMEOUT: Duration = Duration::from_secs(20);
/// How long the shared client waits to open a connection: a dead network fails in this
/// long, not in the request's whole time budget.
pub(crate) const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// The import string's total time, the same as the region and Forever legs get: the site
/// can take tens of seconds to build a realm's string after an ingest tick.
pub(crate) const IMPORT_TIMEOUT: Duration = Duration::from_secs(60);
/// Waits before the first and the second retry of the import string. Two entries = at most
/// two retries, three attempts in all (a timeout allows only one of them, see `retry_delay`).
const RETRY_BACKOFF: [Duration; 2] = [Duration::from_secs(3), Duration::from_secs(10)];
/// A server's `Retry-After` is honoured up to this long.
const RETRY_AFTER_CAP: Duration = Duration::from_secs(30);

pub fn build_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(CLIENT_TIMEOUT)
        .connect_timeout(CONNECT_TIMEOUT)
        .user_agent(concat!("goldcap-companion/", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("reqwest client with only timeouts/user-agent set should always build")
}

/// What to show for a failed request. `reqwest`'s own text for a request error drops the
/// cause, so a timeout, a DNS failure, a refused connection and a TLS error all read
/// "error sending request for url (...)". `limit` is the total time the request was given
/// (the message names it when that ran out).
pub(crate) fn describe_err(e: &reqwest::Error, limit: Duration) -> String {
    if e.is_connect() {
        let cause = if e.is_timeout() {
            format!("timed out after {} s", CONNECT_TIMEOUT.as_secs())
        } else {
            source_chain(e)
        };
        return format!("could not connect: {cause}");
    }
    if e.is_timeout() {
        return format!("timed out after {} s", limit.as_secs());
    }
    let mut own = e.to_string();
    if let Some(url) = e.url() {
        // The address is in the log line's context already; it only makes the text long.
        own = own.replace(&format!(" for url ({url})"), "");
    }
    let chain = source_chain(e);
    if chain.is_empty() {
        own
    } else {
        format!("{own}: {chain}")
    }
}

/// The messages of `e`'s `source()` chain (not `e` itself) joined by ": ", each one only
/// when the one before did not already say it.
fn source_chain(e: &dyn std::error::Error) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut next = e.source();
    while let Some(err) = next {
        let text = err.to_string();
        if !parts.last().is_some_and(|p| p.contains(&text)) {
            parts.push(text);
        }
        next = err.source();
    }
    parts.join(": ")
}

/// What went wrong with one attempt, as far as the retry policy cares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Failure {
    /// The connection could not be opened (refused, unreachable, DNS, TLS, or the
    /// connect timeout). Fails fast, in at most `CONNECT_TIMEOUT`.
    Connect,
    /// The connection was made but the answer did not come in the request's whole time
    /// budget. Each one costs a full budget, so it is retried less than the rest.
    Timeout,
    Status { code: u16, retry_after: Option<Duration> },
}

/// How long to wait before the next attempt, or `None` to give up. `retries_done` is how
/// many retries have already been made; `timed_out_at` is what `retries_done` was when
/// the first attempt timed out, counting the attempt being judged (`None`: none has).
///
/// A failed connection, a 5xx and a 429 are worth two retries; every other status --
/// notably the API's 404 `realm_not_found` / `no_data` -- would answer the same again.
/// A timeout is worth only ONE retry, however the attempts before and after it went: once
/// any attempt has timed out, the retry after it is the last (a site slow enough to time
/// out once rarely recovers within seconds, and each further wait of a whole budget would
/// hold up the Forever leg and config changes queued behind this tick).
fn retry_delay(
    backoff: &[Duration],
    retries_done: usize,
    timed_out_at: Option<usize>,
    failure: Failure,
) -> Option<Duration> {
    if timed_out_at.is_some_and(|at| retries_done > at) {
        return None;
    }
    let base = *backoff.get(retries_done)?;
    match failure {
        Failure::Connect | Failure::Timeout => Some(base),
        Failure::Status { code, retry_after } if code >= 500 || code == 429 => {
            Some(retry_after.map_or(base, |d| d.min(RETRY_AFTER_CAP)))
        }
        Failure::Status { .. } => None,
    }
}

/// Fetches the import string for `region`/`realm`. Only a 2xx response
/// whose body starts with the GCS1 magic prefix counts as success — a
/// non-200 (including the API's own `realm_not_found`/`no_data` 404 bodies)
/// or an unexpected body is a `SyncError`, never written to disk.
///
/// A slow or briefly failing site is retried: up to two more attempts, after 3 s and
/// then 10 s, on a failed connection, a 5xx or a 429 (a `Retry-After` in seconds replaces
/// the wait, up to 30 s) -- but only one more attempt once any attempt has timed out.
///
/// Worst case, by what the site does:
/// - a site that only times out: 60 + 3 + 60 = 123 s (two attempts);
/// - a dead connection: 3 x 10 (connect timeout) + 3 + 10 = 43 s;
/// - a site that answers 5xx / 429 just under the limit each time, and never times out:
///   3 x 60 + 3 + 10 = 193 s, or 3 x 60 + 30 + 30 = 240 s with the longest `Retry-After`
///   (a fast 5xx costs nothing; the slow ones are the unusual case);
/// - the worst mix, two slow 5xx / 429 answers with the longest `Retry-After` and then a
///   timeout: 60 + 30 + 60 + 30 + 60 = 240 s, the same bound -- a timeout never adds a
///   fourth attempt.
pub async fn fetch_import_string(
    client: &reqwest::Client,
    region: &str,
    realm: &str,
) -> Result<String, SyncError> {
    fetch_import_string_at(client, IMPORT_STRING_URL, region, realm, IMPORT_TIMEOUT, &RETRY_BACKOFF)
        .await
}

async fn fetch_import_string_at(
    client: &reqwest::Client,
    url: &str,
    region: &str,
    realm: &str,
    timeout: Duration,
    backoff: &[Duration],
) -> Result<String, SyncError> {
    let mut retries = 0;
    let mut timed_out_at = None;
    loop {
        let (err, failure) =
            match attempt_import_string(client, url, region, realm, timeout).await {
                Ok(body) => return Ok(body),
                Err(failed) => failed,
            };
        if failure == Some(Failure::Timeout) {
            timed_out_at.get_or_insert(retries);
        }
        match failure.and_then(|f| retry_delay(backoff, retries, timed_out_at, f)) {
            Some(wait) => {
                tokio::time::sleep(wait).await;
                retries += 1;
            }
            None => {
                return Err(match err {
                    SyncError::Request(msg) if retries > 0 => {
                        SyncError::Request(format!("{msg} (tried {} times)", retries + 1))
                    }
                    other => other,
                })
            }
        }
    }
}

/// One request and its body read. The `Failure` is set for the errors the retry policy may
/// look at; `None` means this is not something a retry can change.
async fn attempt_import_string(
    client: &reqwest::Client,
    url: &str,
    region: &str,
    realm: &str,
    timeout: Duration,
) -> Result<String, (SyncError, Option<Failure>)> {
    let request_failed = |e: reqwest::Error| {
        // A connect timeout is a failed connection (10 s), not a slow answer (a whole budget).
        let failure = if e.is_connect() {
            Some(Failure::Connect)
        } else if e.is_timeout() {
            Some(Failure::Timeout)
        } else {
            None
        };
        (SyncError::Request(describe_err(&e, timeout)), failure)
    };

    let resp = client
        .get(url)
        .query(&[("region", region), ("realm", realm)])
        .timeout(timeout)
        .send()
        .await
        .map_err(request_failed)?;

    let status = resp.status();
    if !status.is_success() {
        let retry_after = resp
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<u64>().ok())
            .map(Duration::from_secs);
        let code = status.as_u16();
        return Err((SyncError::BadStatus(code), Some(Failure::Status { code, retry_after })));
    }

    let body = resp.text().await.map_err(request_failed)?;
    if !luafile::is_valid_gcs1_body(&body) {
        return Err((SyncError::InvalidBody, None));
    }
    Ok(body)
}

/// Writes a freshly fetched import string -- and the region payload, when there is
/// one to write -- into `{wow_retail_path}/Interface/AddOns/GoldCap_AppData/`.
pub fn apply_import_string(
    wow_retail_path: &Path,
    import_string: &str,
    region_string: Option<&str>,
) -> Result<(), SyncError> {
    let dir = luafile::addon_dir(wow_retail_path);
    luafile::ensure_toc(&dir).map_err(|e| SyncError::Write(e.to_string()))?;
    luafile::write_app_data_lua(&dir, import_string, region_string, luafile::now_unix())
        .map_err(|e| SyncError::Write(e.to_string()))
}

/// Writes a good import string with the region leg's result (`region::refresh`) beside
/// it: a body and its summary as one pair, or neither. The summary comes back only when
/// the file was written: the Status screen must not claim market items that never
/// reached the addon.
pub fn write_prices(
    wow_retail_path: &Path,
    import_string: &str,
    region: Option<(String, crate::region::RegionSummary)>,
) -> Result<Option<crate::region::RegionSummary>, SyncError> {
    let region_body = region.as_ref().map(|(body, _)| body.as_str());
    apply_import_string(wow_retail_path, import_string, region_body)?;
    Ok(region.map(|(_, summary)| summary))
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
        .map_err(|e| describe_err(&e, CLIENT_TIMEOUT))?;
    let slug = match resp.status().as_u16() {
        200 => resp
            .json::<Resolved>()
            .await
            .map_err(|e| describe_err(&e, CLIENT_TIMEOUT))?
            .slug,
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
    // The whole-market payload rides the same write as a passenger: asked only after a
    // good import string, and whatever it does -- 304, failure, too old -- the import
    // string below is written.
    let mut region_summary = None;
    let (fetched_ok, sync_error) = match fetch_result {
        Ok(body) => {
            let region_leg =
                crate::region::refresh(client, &region, logger, luafile::now_unix()).await;
            match write_prices(Path::new(&config.wow_retail_path), &body, region_leg) {
                Ok(written) => {
                    region_summary = written;
                    (true, None)
                }
                Err(e) => (true, Some(e)),
            }
        }
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
            s.region = region_summary;
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

    // Buy runs ride the same passenger rule as the ledger summary leg just
    // above: logger-only, never touches SyncStatus or the tray label. A
    // failed fetch (or a write failure) leaves the previously written
    // Runs.lua on disk untouched — apply_fetch_result performs no
    // filesystem operation on Err. Unpaired = silent no-op, same rule as
    // the other passengers.
    if !config.companion_token.trim().is_empty() {
        let fetched = crate::runs::fetch_runs(client, &config.companion_token).await;
        let failed = fetched.as_ref().err().cloned();
        match crate::runs::apply_fetch_result(
            &luafile::addon_dir(Path::new(&config.wow_retail_path)),
            fetched,
            luafile::now_unix(),
        ) {
            Ok(true) => logger.info("runs: written"),
            Ok(false) => {}
            Err(e) => {
                if failed.as_deref() == Some(e.as_str()) {
                    logger.error(&format!("runs fetch failed: {e}"));
                } else {
                    logger.error(&format!("runs write failed: {e}"));
                }
            }
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

    // Item names the client resolved for the site: same passenger rule.
    crate::upload::upload_item_names_once(
        client,
        &config.companion_token,
        Path::new(&config.wow_retail_path),
        &state_path.join(crate::upload::STATE_FILE_NAME),
        logger,
    )
    .await;

    // Owned lots (My-auctions): same passenger rule as the two uploads above.
    crate::upload::upload_owned_lots_once(
        client,
        &config.companion_token,
        &region,
        Path::new(&config.wow_retail_path),
        &state_path.join(crate::upload::STATE_FILE_NAME),
        logger,
    )
    .await;
}

/// One full tick: the Retail leg (`sync_once` — import string, region data, ledger upload, runs,
/// retail SavedVariables) only when `retail_enabled`, then the Forever leg (`sync_forever`) only
/// when `forever_enabled`. A disabled game is skipped outright rather than run and hidden — for
/// Retail that matters even functionally: an unconfigured `sync_once` would otherwise publish
/// "not configured" into `SyncStatus`, which the tray label and the Status screen would both
/// read as a real error rather than a game the player never turned on. Extracted from the loop
/// below so a disabled game's silence can be tested without spinning up the whole interval timer.
async fn run_tick(
    client: &reqwest::Client,
    config: &Config,
    status: &Arc<Mutex<SyncStatus>>,
    logger: &Logger,
    state_dir: &Path,
) {
    if config.retail_enabled {
        sync_once(client, config, status, logger, state_dir).await;
    }
    if config.forever_enabled {
        crate::forever::sync_forever(client, config, logger, state_dir).await;
    }
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
                    run_tick(&client, &config, &status, &logger, &state_dir).await;
                    on_tick();
                }
                maybe = trigger_rx.recv() => {
                    if maybe.is_none() {
                        return; // sender dropped — app is shutting down
                    }
                    run_tick(&client, &config, &status, &logger, &state_dir).await;
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

        apply_import_string(&dir, "GCS1;eu;dentarg;1;abc", None).unwrap();

        let addon_dir = dir.join("Interface").join("AddOns").join("GoldCap_AppData");
        assert!(addon_dir.join("GoldCap_AppData.toc").exists());
        assert!(addon_dir.join("AppData.lua").exists());

        std::fs::remove_dir_all(&dir).ok();
    }

    // A disabled game must be silent, not report "not configured" as though the player forgot
    // to finish setup — that would flip the tray label to "error: ..." for someone who simply
    // turned Retail off to play Forever-only.
    #[tokio::test]
    async fn a_disabled_retail_leg_is_never_run() {
        let client = build_client();
        let config = Config { retail_enabled: false, forever_enabled: false, ..Config::default() };
        let status = Arc::new(Mutex::new(SyncStatus::default()));
        let dir = std::env::temp_dir()
            .join(format!("goldcap-companion-run-tick-off-{}", std::process::id()));
        let logger = Logger::new(&dir).unwrap();

        run_tick(&client, &config, &status, &logger, &dir).await;

        let s = status.lock().unwrap();
        assert!(s.last_error.is_none(), "a disabled game is quiet, not an error");
        assert!(s.last_attempt_at.is_none(), "sync_once itself must never have run");
        std::fs::remove_dir_all(&dir).ok();
    }

    // The inverse: an enabled-but-unconfigured Retail leg still reports the error it always
    // did — turning games on/off must not accidentally swallow a real "not configured" case.
    #[tokio::test]
    async fn an_enabled_but_unconfigured_retail_leg_still_reports_its_error() {
        let client = build_client();
        let config = Config { retail_enabled: true, forever_enabled: false, ..Config::default() };
        let status = Arc::new(Mutex::new(SyncStatus::default()));
        let dir = std::env::temp_dir()
            .join(format!("goldcap-companion-run-tick-on-{}", std::process::id()));
        let logger = Logger::new(&dir).unwrap();

        run_tick(&client, &config, &status, &logger, &dir).await;

        let s = status.lock().unwrap();
        assert!(s.last_error.is_some());
        std::fs::remove_dir_all(&dir).ok();
    }

    // The Forever leg must not be gated open by a config that only turned Retail on — otherwise
    // a Retail-only player who never touched Forever would still have it probed every tick.
    #[tokio::test]
    async fn a_disabled_forever_leg_never_touches_a_forever_install_that_exists_on_disk() {
        let client = build_client();
        let dir = std::env::temp_dir()
            .join(format!("goldcap-companion-run-tick-forever-off-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let sv = dir
            .join("_classic_beta_")
            .join("WTF")
            .join("Account")
            .join("A")
            .join("SavedVariables");
        std::fs::create_dir_all(&sv).unwrap();
        std::fs::write(
            sv.join("GoldCap.lua"),
            r#"GoldCapDB = { client = { interface = 16001, build = "1.60.1.70009", regionId = 90 } }"#,
        )
        .unwrap();

        let config = Config {
            forever_root_path: dir.to_string_lossy().into_owned(),
            retail_enabled: false,
            forever_enabled: false,
            ..Config::default()
        };
        let status = Arc::new(Mutex::new(SyncStatus::default()));
        let logger = Logger::new(&dir.join("logs")).unwrap();

        run_tick(&client, &config, &status, &logger, &dir).await;

        assert!(
            !dir.join(crate::forever::STATE_FILE_NAME).exists(),
            "forever_enabled=false must skip the Forever leg entirely, even with an install present"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_written_tick_hands_back_the_region_summary() {
        let dir = std::env::temp_dir().join(format!(
            "goldcap-companion-write-prices-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let summary = crate::region::RegionSummary { items: 2, ts: 42 };

        let written = write_prices(
            &dir,
            "GCS1;eu;dentarg;1;abc",
            Some(("GCM1;eu;42;I:1=2,3=4".to_string(), summary)),
        );

        assert_eq!(written.unwrap(), Some(summary));
        let lua = luafile::addon_dir(&dir).join(luafile::LUA_FILE_NAME);
        let contents = std::fs::read_to_string(lua).unwrap();
        assert!(
            contents.contains("regionString = 'GCM1;eu;42;I:1=2,3=4'"),
            "{contents}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_failed_write_claims_no_market_items() {
        // A file where the WoW folder should be: the addon folder cannot be made under it.
        let file = std::env::temp_dir().join(format!(
            "goldcap-companion-write-prices-blocked-{}",
            std::process::id()
        ));
        std::fs::write(&file, "not a folder").unwrap();
        let summary = crate::region::RegionSummary { items: 2, ts: 42 };

        let written = write_prices(
            &file,
            "GCS1;eu;dentarg;1;abc",
            Some(("GCM1;eu;42;I:1=2,3=4".to_string(), summary)),
        );

        assert!(matches!(written, Err(SyncError::Write(_))), "{written:?}");
        std::fs::remove_file(&file).ok();
    }

    // ---- request errors and the import string's retries ----

    use std::sync::atomic::{AtomicUsize, Ordering};

    const GOOD_BODY: &str = "GCS1;eu;dentarg;1;abc";

    fn http(status: &str, extra: &str, body: &str) -> String {
        format!(
            "HTTP/1.1 {status}\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    /// A local server that answers its Nth connection with `script[N]` (the last entry
    /// repeats). `None` = read the request and never answer. Hands back the URL and the
    /// number of connections seen so far.
    fn serve_script(script: Vec<Option<String>>) -> (String, Arc<AtomicUsize>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/v1/addon/import-string", listener.local_addr().unwrap());
        let seen = Arc::new(AtomicUsize::new(0));
        let counter = seen.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let n = counter.fetch_add(1, Ordering::SeqCst);
                let answer = script[n.min(script.len() - 1)].clone();
                std::thread::spawn(move || {
                    let mut request = Vec::new();
                    let mut buf = [0u8; 4096];
                    while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                        match stream.read(&mut buf) {
                            Ok(0) | Err(_) => break,
                            Ok(n) => request.extend_from_slice(&buf[..n]),
                        }
                    }
                    match answer {
                        Some(text) => {
                            let _ = stream.write_all(text.as_bytes());
                        }
                        None => std::thread::sleep(Duration::from_secs(5)),
                    }
                });
            }
        });
        (url, seen)
    }

    async fn request_error(url: &str, timeout: Duration) -> reqwest::Error {
        build_client().get(url).timeout(timeout).send().await.unwrap_err()
    }

    #[tokio::test]
    async fn a_timed_out_request_says_so_and_names_the_limit() {
        let (url, _) = serve_script(vec![None]);
        let e = request_error(&url, Duration::from_secs(1)).await;
        assert!(e.is_timeout());
        assert_eq!(describe_err(&e, Duration::from_secs(1)), "timed out after 1 s");
    }

    #[tokio::test]
    async fn a_refused_connection_says_it_could_not_connect_and_why() {
        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let e = request_error(&format!("http://127.0.0.1:{port}/x"), Duration::from_secs(5)).await;
        let text = describe_err(&e, Duration::from_secs(5));
        assert!(text.starts_with("could not connect: "), "{text}");
        assert!(!text.contains("error sending request"), "{text}");
        assert!(!text.contains("127.0.0.1"), "the address is not repeated: {text}");
        assert!(text.len() > "could not connect: ".len() + 5, "{text}");
    }

    #[tokio::test]
    async fn any_other_error_keeps_its_own_words_and_its_cause_without_the_address() {
        let (url, _) = serve_script(vec![Some(http("200 OK", "", "not json"))]);
        let resp = build_client().get(&url).send().await.unwrap();
        let e = resp.json::<serde_json::Value>().await.unwrap_err();
        assert!(!e.is_timeout() && !e.is_connect());
        let text = describe_err(&e, Duration::from_secs(20));
        assert!(text.starts_with("error decoding response body: "), "{text}");
        assert!(!text.contains("http://"), "{text}");
    }

    const BACKOFF: [Duration; 2] = [Duration::from_secs(3), Duration::from_secs(10)];

    fn status(code: u16, retry_after: Option<u64>) -> Failure {
        Failure::Status { code, retry_after: retry_after.map(Duration::from_secs) }
    }

    #[test]
    fn a_failed_connection_is_retried_twice_after_three_then_ten_seconds() {
        assert_eq!(retry_delay(&BACKOFF, 0, None, Failure::Connect), Some(Duration::from_secs(3)));
        assert_eq!(retry_delay(&BACKOFF, 1, None, Failure::Connect), Some(Duration::from_secs(10)));
        assert_eq!(retry_delay(&BACKOFF, 2, None, Failure::Connect), None);
    }

    #[test]
    fn a_timeout_is_retried_once_after_three_seconds() {
        assert_eq!(retry_delay(&BACKOFF, 0, Some(0), Failure::Timeout), Some(Duration::from_secs(3)));
        assert_eq!(retry_delay(&BACKOFF, 1, Some(0), Failure::Timeout), None);
    }

    #[test]
    fn a_timeout_on_a_later_attempt_still_allows_exactly_one_more() {
        // 5xx first, then a timeout: the retry after the timeout is the last one.
        assert_eq!(retry_delay(&BACKOFF, 1, Some(1), Failure::Timeout), Some(Duration::from_secs(10)));
        assert_eq!(retry_delay(&BACKOFF, 2, Some(1), Failure::Timeout), None);
    }

    #[test]
    fn once_an_attempt_has_timed_out_no_other_failure_gets_a_second_retry() {
        // Timeout first (retry 1 made), then a 5xx / 429 / failed connection: stop.
        for f in [Failure::Connect, status(503, None), status(429, Some(1))] {
            assert_eq!(retry_delay(&BACKOFF, 1, Some(0), f), None, "{f:?}");
        }
        // The same failures before any timeout keep both retries.
        for f in [Failure::Connect, status(503, None), status(429, Some(1))] {
            assert!(retry_delay(&BACKOFF, 1, None, f).is_some(), "{f:?}");
        }
    }

    #[test]
    fn a_5xx_and_a_429_are_retried_but_no_other_status_is() {
        for code in [500, 502, 503, 504, 429] {
            assert_eq!(retry_delay(&BACKOFF, 0, None, status(code, None)), Some(Duration::from_secs(3)), "{code}");
            assert_eq!(retry_delay(&BACKOFF, 2, None, status(code, None)), None, "{code}");
        }
        for code in [400, 401, 403, 404, 410, 422, 304] {
            assert_eq!(retry_delay(&BACKOFF, 0, None, status(code, None)), None, "{code}");
            assert_eq!(retry_delay(&BACKOFF, 0, None, status(code, Some(1))), None, "{code}");
        }
    }

    #[test]
    fn retry_after_replaces_the_wait_but_never_past_thirty_seconds() {
        assert_eq!(retry_delay(&BACKOFF, 0, None, status(429, Some(7))), Some(Duration::from_secs(7)));
        assert_eq!(retry_delay(&BACKOFF, 1, None, status(503, Some(120))), Some(Duration::from_secs(30)));
        assert_eq!(retry_delay(&BACKOFF, 0, None, status(503, Some(0))), Some(Duration::ZERO));
    }

    const FAST: [Duration; 2] = [Duration::from_millis(5), Duration::from_millis(5)];

    async fn fetch(url: &str, timeout: Duration) -> Result<String, SyncError> {
        fetch_import_string_at(&build_client(), url, "eu", "dentarg", timeout, &FAST).await
    }

    #[tokio::test]
    async fn a_404_is_not_retried() {
        let (url, seen) = serve_script(vec![Some(http("404 Not Found", "", r#"{"error":"realm_not_found"}"#))]);
        let out = fetch(&url, Duration::from_secs(5)).await;
        assert!(matches!(out, Err(SyncError::BadStatus(404))), "{out:?}");
        assert_eq!(seen.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_503_then_a_good_answer_comes_through() {
        let (url, seen) = serve_script(vec![
            Some(http("503 Service Unavailable", "", "")),
            Some(http("200 OK", "", GOOD_BODY)),
        ]);
        assert_eq!(fetch(&url, Duration::from_secs(5)).await.unwrap(), GOOD_BODY);
        assert_eq!(seen.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn a_429_with_retry_after_is_honoured_then_retried() {
        let (url, seen) = serve_script(vec![
            Some(http("429 Too Many Requests", "Retry-After: 0\r\n", "")),
            Some(http("200 OK", "", GOOD_BODY)),
        ]);
        assert_eq!(fetch(&url, Duration::from_secs(5)).await.unwrap(), GOOD_BODY);
        assert_eq!(seen.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn a_timeout_is_retried() {
        let (url, seen) = serve_script(vec![None, Some(http("200 OK", "", GOOD_BODY))]);
        assert_eq!(fetch(&url, Duration::from_millis(300)).await.unwrap(), GOOD_BODY);
        assert_eq!(seen.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn a_site_that_keeps_timing_out_gets_two_attempts_and_no_more() {
        let (url, seen) = serve_script(vec![None]);
        let out = fetch(&url, Duration::from_millis(200)).await;
        let Err(SyncError::Request(msg)) = out else { panic!("{out:?}") };
        assert!(msg.starts_with("timed out after "), "{msg}");
        assert!(msg.ends_with("(tried 2 times)"), "{msg}");
        assert_eq!(seen.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn a_timeout_then_a_5xx_is_not_retried_again() {
        let (url, seen) = serve_script(vec![None, Some(http("503 Service Unavailable", "", ""))]);
        let out = fetch(&url, Duration::from_millis(200)).await;
        assert!(matches!(out, Err(SyncError::BadStatus(503))), "{out:?}");
        assert_eq!(seen.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn a_5xx_then_a_timeout_gets_one_more_attempt() {
        let (url, seen) = serve_script(vec![
            Some(http("503 Service Unavailable", "", "")),
            None,
            Some(http("200 OK", "", GOOD_BODY)),
        ]);
        assert_eq!(fetch(&url, Duration::from_millis(200)).await.unwrap(), GOOD_BODY);
        assert_eq!(seen.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn a_5xx_then_two_timeouts_stops_at_three_attempts() {
        let (url, seen) = serve_script(vec![Some(http("503 Service Unavailable", "", "")), None]);
        let out = fetch(&url, Duration::from_millis(200)).await;
        let Err(SyncError::Request(msg)) = out else { panic!("{out:?}") };
        assert!(msg.ends_with("(tried 3 times)"), "{msg}");
        assert_eq!(seen.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn a_site_that_keeps_failing_gets_three_attempts_and_no_more() {
        let (url, seen) = serve_script(vec![Some(http("500 Internal Server Error", "", ""))]);
        let out = fetch(&url, Duration::from_secs(5)).await;
        assert!(matches!(out, Err(SyncError::BadStatus(500))), "{out:?}");
        assert_eq!(seen.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn a_dead_connection_reports_the_cause_and_the_attempts() {
        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let out = fetch(&format!("http://127.0.0.1:{port}/x"), Duration::from_secs(5)).await;
        let Err(SyncError::Request(msg)) = out else { panic!("{out:?}") };
        assert!(msg.starts_with("could not connect: "), "{msg}");
        assert!(msg.ends_with("(tried 3 times)"), "{msg}");
    }

    #[tokio::test]
    async fn a_body_that_is_not_an_import_string_is_not_retried() {
        let (url, seen) = serve_script(vec![Some(http("200 OK", "", "<html>"))]);
        let out = fetch(&url, Duration::from_secs(5)).await;
        assert!(matches!(out, Err(SyncError::InvalidBody)), "{out:?}");
        assert_eq!(seen.load(Ordering::SeqCst), 1);
    }
}
