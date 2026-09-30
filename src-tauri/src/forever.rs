//! WoW: Forever. The player's own auction house scan (`GoldCapDB.foreverScan.fold`) goes up
//! to goldcap.gg, and every player's prices come back into the Forever install's
//! `GoldCap_AppData`. The wire is the site's (plan 2a Tasks 3 and 9; docs/companion/AGENTS.md
//! "WoW: Forever upload contract").

use mlua::{Lua, LuaOptions, StdLib, Table, Value};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

/// WoW: Forever's interface numbers: 16001 in the beta. The same range as the addon's
/// `GC.Game.FOREVER_MIN_INTERFACE`/`FOREVER_MAX_INTERFACE` and the API's `isForeverInterface`.
pub const FOREVER_MIN_INTERFACE: i64 = 16_000;
pub const FOREVER_MAX_INTERFACE: i64 = 16_999;

pub fn is_forever_interface(interface: i64) -> bool {
    (FOREVER_MIN_INTERFACE..=FOREVER_MAX_INTERFACE).contains(&interface)
}

/// `GoldCapDB.client`, as far as discovery needs it.
#[derive(Debug, Clone, PartialEq)]
pub struct Passport {
    pub interface: i64,
    pub build: String,
    pub region_id: Option<i64>,
}

fn globals_db(lua: &Lua, source: &str) -> Option<mlua::Table> {
    lua.load(source).exec().ok()?;
    match lua.globals().get::<Value>("GoldCapDB") {
        Ok(Value::Table(db)) => Some(db),
        _ => None,
    }
}

/// The passport a SavedVariables file carries, or None. Anything unreadable is "no passport".
pub fn read_passport(source: &str) -> Option<Passport> {
    let lua = Lua::new_with(StdLib::NONE, LuaOptions::default()).ok()?;
    let db = globals_db(&lua, source)?;
    let Ok(Value::Table(client)) = db.get::<Value>("client") else { return None };
    let interface = crate::savedvars::opt_int(&client, "interface")?;
    let build = crate::savedvars::opt_string(&client, "build")?.trim().to_string();
    if !crate::savedvars::is_valid_passport_build(&build) {
        return None;
    }
    Some(Passport { interface, build, region_id: crate::savedvars::opt_int(&client, "regionId") })
}

/// The site's own bounds (plan 2a Task 3): a fold past them is refused whole, an item past
/// them alone.
pub const MAX_ITEMS: usize = 20_000;
pub const MAX_ITEM_CHARS: usize = 256;
/// The whole request body, uncompressed — a fold past this is refused whole by the server
/// (413). Checked locally before sending so a slow uplink's 20 s request timeout cannot turn a
/// body this big into an endless Retry loop that resends it every tick (fix round 1 M5,
/// mirroring FOREVER_SCAN_MAX_BYTES in packages/api-contract).
pub const MAX_UPLOAD_BYTES: usize = 2 * 1024 * 1024;
const SOURCES: [&str; 3] = ["replicate", "replicate+browse", "browse"];
const FACTIONS: [&str; 3] = ["Horde", "Alliance", "Neutral"];

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ForeverClient {
    pub interface: i64,
    pub build: String,
    pub region_id: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ForeverFold {
    pub v: i64,
    pub at: i64,
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rows: Option<i64>,
    pub item_count: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub partial: Option<bool>,
    pub region: i64,
    pub realm: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub faction: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ruleset: Option<String>,
    pub build: String,
    pub interface: i64,
    /// Item id (decimal string) → the addon's own encoded string, verbatim.
    pub items: BTreeMap<String, String>,
}

/// `POST /v1/forever/scans`'s body, exactly: `{ client, fold }`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ForeverUpload {
    pub client: ForeverClient,
    pub fold: ForeverFold,
}

/// Postgres `int4` max: the server drops an id above this as unknown (it can never exist in
/// the catalogue, whose lookup array is also int4) rather than let the query crash — mirrored
/// here so the companion drops the same items locally instead of sending them for nothing.
const ITEM_MAX_ID: i64 = 2_147_483_647;

fn item_id(key: &Value) -> Option<i64> {
    match key {
        Value::Integer(i) if (1..=ITEM_MAX_ID).contains(i) => Some(*i),
        Value::Number(n) if n.fract() == 0.0 && *n >= 1.0 && *n <= ITEM_MAX_ID as f64 => Some(*n as i64),
        _ => None,
    }
}

fn read_items(fold: &Table) -> BTreeMap<String, String> {
    let mut items = BTreeMap::new();
    let Ok(Value::Table(t)) = fold.get::<Value>("items") else { return items };
    for pair in t.pairs::<Value, Value>() {
        let Ok((key, Value::String(s))) = pair else { continue };
        let (Some(id), Ok(s)) = (item_id(&key), s.to_str()) else { continue };
        if s.len() <= MAX_ITEM_CHARS {
            items.insert(id.to_string(), s.to_string());
        }
    }
    items
}

/// A fold timestamped further than this from now is a forged or corrupt `at` — the server
/// refuses it outright (400) rather than accept and clamp it, so the companion never sends one
/// (fix round 1 M5, mirroring `FOREVER_FOLD_MAX_CLOCK_SKEW_SEC` in packages/api-contract).
const FOLD_MAX_CLOCK_SKEW_SECS: i64 = 10 * 365 * 24 * 3600;

/// The server's `NO_CONTROL_CHARS_RE`: a bare C0 control character or DEL anywhere in the
/// string. Realm and ruleset are stored, logged and turned into a URL slug, and one of these
/// used to reach an INSERT and crash it server-side.
fn has_control_char(s: &str) -> bool {
    s.chars().any(|c| (c as u32) <= 0x1f || c as u32 == 0x7f)
}

/// The Forever upload one SavedVariables file holds, or None: no Forever passport, no fold,
/// or a fold the site would refuse whole (no items, an unknown source, no realm, a corrupt or
/// out-of-range `at`). A courier: the items go as the addon wrote them, and the site checks
/// their grammar; a `ruleset` with a control character is dropped alone rather than take the
/// whole fold down with it, exactly as `faction` already is.
pub fn parse_forever_upload(source: &str) -> Result<Option<ForeverUpload>, String> {
    let lua = Lua::new_with(StdLib::NONE, LuaOptions::default()).map_err(|e| e.to_string())?;
    lua.load(source).exec().map_err(|e| e.to_string())?;
    let Ok(Value::Table(db)) = lua.globals().get::<Value>("GoldCapDB") else { return Ok(None) };
    let Ok(Value::Table(c)) = db.get::<Value>("client") else { return Ok(None) };
    let (Some(interface), Some(build), Some(region_id)) = (
        crate::savedvars::opt_int(&c, "interface"),
        crate::savedvars::opt_string(&c, "build").map(|b| b.trim().to_string()),
        crate::savedvars::opt_int(&c, "regionId"),
    ) else {
        return Ok(None);
    };
    if !is_forever_interface(interface) || !crate::savedvars::is_valid_passport_build(&build) || !(1..=999).contains(&region_id) {
        return Ok(None);
    }
    let Ok(Value::Table(scan)) = db.get::<Value>("foreverScan") else { return Ok(None) };
    let Ok(Value::Table(f)) = scan.get::<Value>("fold") else { return Ok(None) };

    use crate::savedvars::{flag, opt_int, opt_string};
    let (Some(v), Some(at), Some(source), Some(item_count), Some(region), Some(realm), Some(fold_build), Some(fold_interface)) = (
        opt_int(&f, "v"),
        opt_int(&f, "at"),
        opt_string(&f, "source"),
        opt_int(&f, "itemCount"),
        opt_int(&f, "region"),
        opt_string(&f, "realm").map(|r| r.trim().to_string()),
        opt_string(&f, "build"),
        opt_int(&f, "interface"),
    ) else {
        return Ok(None);
    };
    let items = read_items(&f);
    // `at` mirrors the server's own bounds: positive, and no more than ten years from now — a
    // NaN or an out-of-range Lua number saturates through `opt_int`'s `as i64` cast to a value
    // far outside this window anyway, so no separate finiteness check is needed.
    if items.is_empty()
        || items.len() > MAX_ITEMS
        || !SOURCES.contains(&source.as_str())
        || realm.is_empty()
        || realm.chars().count() > 64
        || at <= 0
        || (at - crate::luafile::now_unix()).abs() > FOLD_MAX_CLOCK_SKEW_SECS
    {
        return Ok(None);
    }
    Ok(Some(ForeverUpload {
        client: ForeverClient { interface, build, region_id },
        fold: ForeverFold {
            v,
            at,
            source,
            rows: opt_int(&f, "rows"),
            item_count,
            partial: flag(&f, "partial").then_some(true),
            region,
            realm,
            faction: opt_string(&f, "faction").filter(|x| FACTIONS.contains(&x.as_str())),
            ruleset: opt_string(&f, "ruleset")
                .map(|r| r.trim().to_string())
                .filter(|r| !r.is_empty() && r.chars().count() <= 32 && !has_control_char(r)),
            build: fold_build,
            interface: fold_interface,
            items,
        },
    }))
}

pub const API_BASE: &str = "https://api.goldcap.gg";
pub const STATE_FILE_NAME: &str = "forever.json";

#[derive(Debug, Clone, PartialEq)]
pub enum UploadOutcome {
    /// The site has had its say about this fold; it is never sent again (Decision E5).
    /// `market`: the market the fold was filed under — only ever set from an `accepted` or
    /// `duplicate` answer (fix round 1 M7). E6 wants the latest *accepted* fold's market; a
    /// `quarantined` or `rejected` answer can name a market that is not this install's own (a
    /// stale fold from another character's realm), and must not switch it.
    /// `note`: what the Status screen should tell the player, when anything.
    /// `impact`: what the accepted scan changed on its market — `(updated, only_yours,
    /// first_on_market)` — only from an `accepted` or `duplicate` answer that carried it whole.
    /// Anything malformed, or an old site that sends none, is `None`, never a wrong number.
    Done { market: Option<String>, note: Option<String>, impact: Option<(u32, u32, bool)> },
    /// The token itself was refused (401): nothing decided about this fold, so it is retried
    /// next tick like any other unresolved answer — but distinct from `Retry` so the caller can
    /// stop telling the addon this install uploads until a later attempt actually succeeds
    /// (fix round 1 M8).
    Unauthorized,
    /// Nothing decided: the next tick sends it again.
    Retry(String),
}

fn note_for(status: &str, reason: Option<&str>) -> Option<String> {
    match (status, reason) {
        ("quarantined", Some("unlinked")) => Some("Link Battle.net on goldcap.gg for your scans to count in public prices".into()),
        ("quarantined", Some(_)) => Some("Your scans are not counted in public prices right now".into()),
        ("rejected", Some("stale")) => Some("The last scan was over a day old when it was sent".into()),
        ("rejected", Some(r)) => Some(format!("goldcap.gg refused the last scan ({r})")),
        _ => None,
    }
}

/// The site's `impact` object, read strictly: `updated` and `onlyYours` whole non-negative
/// numbers that fit u32, `onlyYours <= updated`, `firstOnMarket` a bool. One thing off and the
/// whole object is dropped — the Status card would rather say nothing than a wrong number.
fn read_impact(body: &serde_json::Value) -> Option<(u32, u32, bool)> {
    let impact = body.get("impact")?;
    let updated = u32::try_from(impact.get("updated")?.as_u64()?).ok()?;
    let only_yours = u32::try_from(impact.get("onlyYours")?.as_u64()?).ok()?;
    let first = impact.get("firstOnMarket")?.as_bool()?;
    (only_yours <= updated).then_some((updated, only_yours, first))
}

/// Sends one fold to `POST {base}/v1/forever/scans` under the pairing token, with the Forever
/// passport as `X-GoldCap-Client` (plan 2a Task 3).
pub async fn upload_fold(client: &reqwest::Client, base: &str, token: &str, up: &ForeverUpload) -> UploadOutcome {
    let sent = client
        .post(format!("{base}/v1/forever/scans"))
        .bearer_auth(token)
        .header(crate::upload::CLIENT_HEADER, format!("{}/{}", up.client.interface, up.client.build))
        .json(up)
        .send()
        .await;
    let res = match sent {
        Ok(r) => r,
        Err(e) => return UploadOutcome::Retry(crate::sync::describe_err(&e, crate::sync::CLIENT_TIMEOUT)),
    };
    let code = res.status().as_u16();
    let body: serde_json::Value = res.json().await.unwrap_or(serde_json::Value::Null);
    let status = body["status"].as_str();
    let market = status
        .filter(|s| matches!(*s, "accepted" | "duplicate"))
        .and_then(|_| body["market"].as_str())
        .map(str::to_string);
    let impact = status
        .filter(|s| matches!(*s, "accepted" | "duplicate"))
        .and_then(|_| read_impact(&body));
    match code {
        // A captive portal or a proxy's own error page can answer 200 with a body that carries
        // no `status` at all — that is not the site having its say, so the fold is retried
        // rather than silently marked sent and lost (fix round 1 M4).
        200 if status.is_none() => UploadOutcome::Retry("200 without a status".into()),
        200 | 422 => UploadOutcome::Done { market, note: note_for(status.unwrap_or(""), body["reason"].as_str()), impact },
        400 | 409 | 413 => UploadOutcome::Done {
            market: None,
            note: Some(format!("goldcap.gg refused the last scan ({})", body["error"].as_str().unwrap_or("unknown"))),
            impact: None,
        },
        401 => UploadOutcome::Unauthorized,
        _ => UploadOutcome::Retry(format!("status {code}")),
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct InstallState {
    pub market: Option<String>,
    /// The realm and faction the last *accepted or duplicate* fold named — same rule as
    /// `market` (fix round 1 M7): a quarantined or rejected answer can carry a stale fold's
    /// realm and must not overwrite what a good fold last said. For the Status screen's
    /// Forever card ("realm · faction"), computed here rather than parsed back out of `market`
    /// (a URL slug) in JS.
    pub realm: Option<String>,
    pub faction: Option<String>,
    pub last_scan_at: Option<i64>,
    pub last_sent_at: Option<i64>,
    /// How many items the last *sent* fold carried — the Status screen's "scan uploaded <age> ·
    /// <items> items" line. Set alongside `last_sent_at`, so the two always describe the same
    /// fold.
    pub sent_items: Option<u32>,
    pub note: Option<String>,
    pub crowd_items: Option<u32>,
    pub crowd_ts: Option<i64>,
    /// When this install's `AppData.lua` last actually carried a `foreverString` — distinct
    /// from `crowd_ts` (the site's own snapshot time for that payload): this is local write
    /// time, what "crowd prices written to the addon <age>" on the Status screen means.
    pub crowd_written_at: Option<i64>,
    /// The last upload attempt got a 401: the token is no longer good, so `AppData.lua` must
    /// say `foreverUpload = false` until a fresh upload actually succeeds — the addon's "shared
    /// on your next /reload" line must not lie about a revoked token (fix round 1 M8).
    pub unauthorized: bool,
    /// What the last *sent* fold changed on its market, for the Status card. Set on every `Done`
    /// (None when the answer carried no numbers), so the card never shows an older scan's
    /// numbers under a newer "Scan uploaded" line — the rule `sent_items` follows.
    pub impact: Option<ScanImpact>,
}

/// What one accepted scan changed on its market, as goldcap.gg counted it at receipt. `at` is
/// the fold's own `at`: the key the addon matches its own scan by.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanImpact {
    pub at: i64,
    pub sent_at: i64,
    pub updated: u32,
    pub only_yours: u32,
    pub first: bool,
    pub realm: String,
    pub faction: Option<String>,
}

/// One row of the Status screen's game list.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GameStatus {
    pub folder: String,
    pub game: crate::games::GameKind,
    #[serde(flatten)]
    pub state: InstallState,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ForeverState {
    /// SavedVariables path → the keys of BUY purchases goldcap.gg has taken from that file (BUY 2.0).
    /// Only keys still in the file are kept, so this never outgrows the file.
    pub purchases_sent: BTreeMap<String, std::collections::BTreeSet<String>>,
    /// SavedVariables path → the file's (mtime, length) when every purchase in it had been sent:
    /// an unchanged file is not parsed again for purchases.
    pub purchase_stamps: BTreeMap<String, (i64, u64)>,
    /// SavedVariables path → the fold `at` the site has had its say about.
    pub sent: BTreeMap<String, i64>,
    /// Install dir → what the Forever leg knows about it.
    pub installs: BTreeMap<String, InstallState>,
    /// Every game folder the last tick found, for the Status screen.
    pub games: Vec<GameStatus>,
    /// SavedVariables path → the last scan result for that file (one WoW account), as
    /// `foreverImpact` for the addon. Removed when the last answer for the file had no numbers.
    pub impacts: BTreeMap<String, ScanImpact>,
}

impl ForeverState {
    pub fn load_from(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    /// Atomic (temp + rename): `get_status` reads this file every 5 s, so a torn write would
    /// flash the games list empty, and a crash mid-write would lose `sent`, re-sending every
    /// current fold once (fix round 1 M1).
    pub fn save_to(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(self).map_err(std::io::Error::from)?;
        crate::luafile::write_atomic(path, &text)
    }
}

/// Older than this by its own scan time, and the addon's tooltip would print a days-old price as
/// "AH value": the site sends nothing older (plan 2a D15), and the companion writes nothing older.
pub const CROWD_MAX_AGE_SECS: i64 = 72 * 3600;
/// The site builds at most 2,000,000 characters (plan 2a Task 9).
pub const CROWD_MAX_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CrowdSummary {
    pub items: u32,
    pub ts: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct KeptCrowd {
    pub slug: String,
    pub body: String,
    pub etag: Option<String>,
    pub summary: CrowdSummary,
}

/// `GCF1;<slug>;<regionId>;<realm>;<faction>;<ts>;I:…` for exactly this market, with items.
pub fn summarize_gcf1(body: &str, slug: &str) -> Result<CrowdSummary, String> {
    if body.len() > CROWD_MAX_BYTES {
        return Err("Forever prices too large".into());
    }
    let rest = body
        .strip_prefix(&format!("GCF1;{slug};"))
        .ok_or_else(|| format!("not the Forever prices of {slug}"))?;
    let fields: Vec<&str> = rest.splitn(5, ';').collect();
    let [_, _, _, ts, sections] = fields[..] else { return Err("Forever prices have no header".into()) };
    let ts: i64 = ts.parse().map_err(|_| "Forever prices have no time".to_string())?;
    let items = sections
        .split(';')
        .find_map(|s| s.strip_prefix("I:"))
        .map(|t| if t.is_empty() { 0 } else { t.split(',').count() as u32 })
        .unwrap_or(0);
    if items == 0 {
        return Err("Forever prices carried no items".into());
    }
    Ok(CrowdSummary { items, ts })
}

/// Kept Forever payloads, by install dir, for the life of the process.
pub fn kept_crowd_store() -> &'static std::sync::Mutex<std::collections::HashMap<String, KeptCrowd>> {
    static KEPT: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, KeptCrowd>>> = std::sync::OnceLock::new();
    KEPT.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

/// One install's prices-back leg: revalidate or fetch `GET {base}/v1/forever/addon-data?market=`,
/// keep the last good body, and hand back the body to write with its summary — or None when
/// nothing is kept, or what is kept is older than CROWD_MAX_AGE_SECS. Never an error: a failure
/// is a log line.
pub async fn refresh_crowd_at(
    client: &reqwest::Client,
    base: &str,
    store: &std::sync::Mutex<std::collections::HashMap<String, KeptCrowd>>,
    install_key: &str,
    slug: &str,
    logger: &crate::logging::Logger,
    now: i64,
) -> Option<(String, CrowdSummary)> {
    let previous = store.lock().unwrap_or_else(|p| p.into_inner()).remove(install_key).filter(|k| k.slug == slug);
    let etag = previous.as_ref().and_then(|k| k.etag.clone());
    let request = client
        .get(format!("{base}/v1/forever/addon-data"))
        .query(&[("market", slug)]);
    let fetched =
        crate::region::fetch_capped(request, std::time::Duration::from_secs(60), etag.as_deref()).await;
    let kept = match fetched {
        Ok(crate::region::Fetched::Fresh { body, etag }) => match summarize_gcf1(&body, slug) {
            Ok(summary) => Some(KeptCrowd { slug: slug.to_string(), body, etag, summary }),
            Err(e) => {
                logger.error(&format!("forever prices {slug}: {e}"));
                previous
            }
        },
        Ok(crate::region::Fetched::NotModified) => previous,
        Err(e) => {
            logger.error(&format!("forever prices {slug} failed: {e}"));
            previous
        }
    };
    let write = kept
        .as_ref()
        .filter(|k| now - k.summary.ts <= CROWD_MAX_AGE_SECS)
        .map(|k| (k.body.clone(), k.summary));
    if let Some(k) = kept {
        store.lock().unwrap_or_else(|p| p.into_inner()).insert(install_key.to_string(), k);
    }
    write
}

/// One tick's Forever leg, against goldcap.gg, remembering in `{state_dir}/forever.json`.
/// Runs after the retail `sync_once` in `sync::run_loop`, as its own passenger: it never touches
/// SyncStatus or the tray, and a failure is a log line (Decision E8).
pub async fn sync_forever(client: &reqwest::Client, config: &crate::config::Config, logger: &crate::logging::Logger, state_dir: &Path) {
    let _ = sync_forever_at(client, API_BASE, config, logger, &state_dir.join(STATE_FILE_NAME), kept_crowd_store(), crate::luafile::now_unix()).await;
}

pub async fn sync_forever_at(
    client: &reqwest::Client,
    base: &str,
    config: &crate::config::Config,
    logger: &crate::logging::Logger,
    state_path: &Path,
    store: &std::sync::Mutex<std::collections::HashMap<String, KeptCrowd>>,
    now: i64,
) -> ForeverState {
    match crate::games::forever_root(config) {
        Some(root) => sync_forever_at_root(client, base, &root, config, logger, state_path, store, now).await,
        None => ForeverState::load_from(state_path),
    }
}

type ParsedCache = std::collections::HashMap<String, (i64, u64, Option<ForeverUpload>)>;

/// (SavedVariables path → (mtime, byte length, the parse result)), for the life of the
/// process. Shared by every install: paths are absolute, so two installs never collide.
fn parsed_cache() -> &'static std::sync::Mutex<ParsedCache> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<ParsedCache>> = std::sync::OnceLock::new();
    CACHE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

fn file_stamp(path: &Path) -> Option<(i64, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    Some((mtime, meta.len()))
}

/// `parse_forever_upload`, cached by the file's (mtime, length): a tick where nothing on disk
/// changed since the fold was last looked at parses nothing at all — no read, no Lua interpreter
/// (fix round 1 M2). A file that cannot be stat'd or read right now is never cached, so the next
/// tick retries it; a file that parses to "no valid fold" (an unknown shape) IS cached as such,
/// so a genuinely bad file is not re-parsed every tick either.
fn parse_forever_upload_cached(file: &Path, logger: &crate::logging::Logger) -> Option<ForeverUpload> {
    let stamp = file_stamp(file)?;
    let file_key = file.to_string_lossy().into_owned();
    let cached = parsed_cache()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&file_key)
        .filter(|(m, l, _)| (*m, *l) == stamp)
        .map(|(_, _, up)| up.clone());
    if let Some(up) = cached {
        return up;
    }
    let src = std::fs::read_to_string(file).ok()?;
    let parsed = match parse_forever_upload(&src) {
        Ok(v) => v,
        Err(e) => {
            logger.error(&format!("{}: not readable yet ({e})", file.display()));
            return None;
        }
    };
    parsed_cache()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert(file_key, (stamp.0, stamp.1, parsed.clone()));
    parsed
}

#[allow(clippy::too_many_arguments)]
pub async fn sync_forever_at_root(
    client: &reqwest::Client,
    base: &str,
    root: &Path,
    config: &crate::config::Config,
    logger: &crate::logging::Logger,
    state_path: &Path,
    store: &std::sync::Mutex<std::collections::HashMap<String, KeptCrowd>>,
    now: i64,
) -> ForeverState {
    use crate::games::GameKind;
    let mut state = ForeverState::load_from(state_path);
    let retail = config.wow_retail_path.trim();
    let retail_dir = (!retail.is_empty()).then(|| std::path::PathBuf::from(retail));
    let games = crate::games::discover(root, retail_dir.as_deref());
    let token = config.companion_token.trim().to_string();

    // Never had a Forever install, and none exists now: behave exactly like 1.13.0 — no
    // forever.json is ever created, and the Status screen has no games list to show
    // (fix round 1 I1). A machine that once had one keeps its history even if that install is
    // gone this tick, same as retail's own stage rows keep their last-good state.
    if !games.iter().any(|g| g.kind == GameKind::Forever) && state.installs.is_empty() {
        return state;
    }

    for game in games.iter().filter(|g| g.kind == GameKind::Forever) {
        let key = game.dir.to_string_lossy().into_owned();
        // The max Forever interface seen, not whichever file happened to parse last (read_dir
        // order is arbitrary) — otherwise a second, older account can regress the TOC and make
        // the Forever client treat GoldCap_AppData as out of date (fix round 1 M3).
        let mut interface = game.interface.unwrap_or(FOREVER_MIN_INTERFACE + 1);
        for file in &game.saved_vars {
            let Some(up) = parse_forever_upload_cached(file, logger) else { continue };
            interface = interface.max(up.client.interface);
            let entry = state.installs.entry(key.clone()).or_default();
            entry.last_scan_at = entry.last_scan_at.max(Some(up.fold.at));
            let file_key = file.to_string_lossy().into_owned();
            if token.is_empty() || state.sent.get(&file_key) == Some(&up.fold.at) {
                continue;
            }
            // The server refuses a body over MAX_UPLOAD_BYTES whole (413) anyway, but on a slow
            // uplink the client's own request timeout fires first — a Retry, which would resend
            // this same oversized body every tick forever. Check first and mark it done locally
            // instead (fix round 1 M5).
            let body_len = serde_json::to_vec(&up).map(|b| b.len()).unwrap_or(0);
            if body_len > MAX_UPLOAD_BYTES {
                logger.error(&format!(
                    "forever scan from {}: {body_len} bytes over the {} MiB cap — not sent",
                    game.folder,
                    MAX_UPLOAD_BYTES / (1024 * 1024)
                ));
                state.sent.insert(file_key, up.fold.at);
                let entry = state.installs.entry(key.clone()).or_default();
                entry.note = Some("The last scan was too large to send".into());
                continue;
            }
            match upload_fold(client, base, &token, &up).await {
                UploadOutcome::Done { market, note, impact } => {
                    logger.info(&format!("forever scan from {}: sent ({} items)", game.folder, up.fold.items.len()));
                    state.sent.insert(file_key.clone(), up.fold.at);
                    let scan_impact = impact.map(|(updated, only_yours, first)| ScanImpact {
                        at: up.fold.at,
                        sent_at: now,
                        updated,
                        only_yours,
                        first,
                        realm: up.fold.realm.clone(),
                        faction: up.fold.faction.clone(),
                    });
                    match &scan_impact {
                        Some(i) => state.impacts.insert(file_key, i.clone()),
                        None => state.impacts.remove(&file_key),
                    };
                    let entry = state.installs.entry(key.clone()).or_default();
                    entry.impact = scan_impact;
                    entry.last_sent_at = Some(now);
                    entry.note = note;
                    entry.unauthorized = false;
                    entry.sent_items = Some(up.fold.items.len() as u32);
                    if market.is_some() {
                        entry.market = market;
                        entry.realm = Some(up.fold.realm.clone());
                        entry.faction = up.fold.faction.clone();
                    }
                }
                UploadOutcome::Unauthorized => {
                    logger.error(&format!("forever scan from {}: token refused (401)", game.folder));
                    let entry = state.installs.entry(key.clone()).or_default();
                    entry.unauthorized = true;
                }
                UploadOutcome::Retry(why) => logger.error(&format!("forever scan from {}: will retry ({why})", game.folder)),
            }
        }
        // BUY 2.0: purchases made through the BUY tab in this install go up to the Forever route —
        // never a retail one. A file is parsed again only when it changed since everything in it
        // was sent.
        if !token.is_empty() && !state.installs.get(&key).is_some_and(|e| e.unauthorized) {
            for file in &game.saved_vars {
                let file_key = file.to_string_lossy().into_owned();
                let stamp = file_stamp(file);
                if stamp.is_some() && state.purchase_stamps.get(&file_key).copied() == stamp {
                    continue;
                }
                let mut sent = state.purchases_sent.remove(&file_key).unwrap_or_default();
                let pass = crate::forever_runs::send_run_purchases(client, base, &token, file, &mut sent, logger).await;
                state.purchases_sent.insert(file_key.clone(), sent);
                match pass {
                    crate::forever_runs::PurchasePass::Complete => {
                        if let Some(s) = stamp {
                            state.purchase_stamps.insert(file_key, s);
                        }
                    }
                    crate::forever_runs::PurchasePass::Incomplete => {}
                    crate::forever_runs::PurchasePass::Unauthorized => {
                        state.installs.entry(key.clone()).or_default().unauthorized = true;
                        break;
                    }
                }
            }
        }
        let market = state.installs.get(&key).and_then(|e| e.market.clone());
        let crowd = match &market {
            Some(slug) => refresh_crowd_at(client, base, store, &key, slug, logger, now).await,
            None => None,
        };
        let entry = state.installs.entry(key.clone()).or_default();
        entry.crowd_items = crowd.as_ref().map(|(_, s)| s.items);
        entry.crowd_ts = crowd.as_ref().map(|(_, s)| s.ts);
        // A revoked token that 401s every tick must not keep promising "shared on your next
        // /reload" — the addon only prints that while `foreverUpload == true` (fix round 1 M8).
        let can_upload = !token.is_empty() && !entry.unauthorized;
        let has_crowd = crowd.is_some();
        let impacts: Vec<ScanImpact> = game
            .saved_vars
            .iter()
            .filter_map(|f| state.impacts.get(f.to_string_lossy().as_ref()).cloned())
            .collect();
        match crate::luafile::write_forever_app_data(
            &crate::luafile::addon_dir(&game.dir),
            interface,
            crowd.as_ref().map(|(body, _)| body.as_str()),
            can_upload,
            &impacts,
            now,
        ) {
            Ok(()) => {
                if has_crowd {
                    state.installs.entry(key.clone()).or_default().crowd_written_at = Some(now);
                }
            }
            Err(e) => logger.error(&format!("forever prices for {}: could not write ({e})", game.folder)),
        }
        // BUY 2.0: this account's WoW: Forever lists, priced in this install's market, as Runs.lua
        // beside AppData.lua. Only while paired and trusted; a failure keeps the previous file.
        if can_upload {
            let fetched = crate::runs::fetch_forever_runs(client, base, &token, market.as_deref()).await;
            if let Err(e) = crate::runs::apply_forever_fetch_result(&crate::luafile::addon_dir(&game.dir), fetched, now) {
                logger.error(&format!("forever runs for {}: {e}", game.folder));
            }
        }
    }

    state.games = games
        .iter()
        .map(|g| GameStatus {
            folder: g.folder.clone(),
            game: g.kind,
            state: state.installs.get(&g.dir.to_string_lossy().into_owned()).cloned().unwrap_or_default(),
        })
        .collect();
    if let Err(e) = state.save_to(state_path) {
        logger.error(&format!("forever state not saved: {e}"));
    }
    state
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    const REAL: &str = include_str!("../tests/fixtures/GoldCap-forever.lua");

    #[test]
    fn the_owners_real_beta_fold_becomes_the_upload_body() {
        let up = parse_forever_upload(REAL).unwrap().expect("a Forever fold");
        assert_eq!(up.client, ForeverClient { interface: 16001, build: "1.60.1.70009".into(), region_id: 90 });
        assert_eq!(up.fold.items.len(), 1974);
        assert_eq!(up.fold.items["2589"], "67,7225,289,;0x53 1x156 1x24 1x514 4x270|8,12,49");
        assert_eq!(up.fold.items["6451"], "3000,78,,b;");
        assert_eq!(up.fold.realm, "Classic Beta PvE 2");
        assert_eq!(up.fold.faction.as_deref(), Some("Horde"));
        assert_eq!((up.fold.v, up.fold.region, up.fold.interface, up.fold.rows), (2, 90, 16001, Some(72080)));
        assert_eq!(up.fold.source, "replicate+browse");
    }

    #[test]
    fn the_body_is_exactly_the_contract_and_carries_nothing_else() {
        let up = parse_forever_upload(REAL).unwrap().unwrap();
        let json = serde_json::to_value(&up).unwrap();
        let top: Vec<&str> = json.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(top, vec!["client", "fold"]);
        assert_eq!(json["client"], serde_json::json!({ "interface": 16001, "build": "1.60.1.70009", "regionId": 90 }));
        let mut fold_keys: Vec<&str> = json["fold"].as_object().unwrap().keys().map(String::as_str).collect();
        fold_keys.sort();
        assert_eq!(fold_keys, vec!["at", "build", "faction", "interface", "itemCount", "items", "realm", "region", "rows", "source", "v"]);
        let text = json.to_string();
        for leak in ["ledger", "liveObservations", "fixture-sale-1", "itemName", "\"region\":\"us\""] {
            assert!(!text.contains(leak), "{leak} must never ride a Forever upload");
        }
    }

    #[test]
    fn the_owners_forever_file_never_reaches_a_retail_route() {
        // The retail parser sees what is there — a ledger row and a live observation, region "us" —
        let data = crate::savedvars::parse_saved_variables(REAL).unwrap();
        assert_eq!(data.entries.len(), 1);
        assert_eq!(data.entries[0].region.as_deref(), Some("us"));
        assert_eq!(data.observations.len(), 1);
        // — and the passport keeps every one of them off the retail routes.
        assert!(!crate::upload::uploadable(&data));
    }

    #[test]
    fn a_retail_file_is_never_a_forever_upload() {
        let retail = r#"GoldCapDB = { client = { interface = 120100, build = "12.1.0.69933", regionId = 3 },
            foreverScan = { fold = { v = 2, at = 1, source = "replicate", itemCount = 1, region = 3, realm = "X",
            build = "12.1.0.69933", interface = 120100, items = { [1] = "1,1,1,;0x1" } } } }"#;
        assert_eq!(parse_forever_upload(retail).unwrap(), None);
        assert_eq!(parse_forever_upload(r#"GoldCapDB = { ledger = {} }"#).unwrap(), None, "no passport");
    }

    #[test]
    fn a_fold_the_server_would_refuse_whole_is_not_sent() {
        let with = |fold: &str| format!(r#"GoldCapDB = {{ client = {{ interface = 16001, build = "1.60.1.70009", regionId = 90 }}, foreverScan = {{ fold = {{ {fold} }} }} }}"#);
        let base = r#"v = 2, at = 1790464249, source = "replicate", itemCount = 1, region = 90, realm = "R", build = "1.60.1.70009", interface = 16001"#;
        // No items, a source the site does not know, an empty realm: never valid, so None.
        assert_eq!(parse_forever_upload(&with(&format!("{base}, items = {{}}"))).unwrap(), None);
        assert_eq!(parse_forever_upload(&with(&base.replace("\"replicate\"", "\"dump\"").replace(", itemCount", ", items = { [1] = \"1,1,1,;0x1\" }, itemCount"))).unwrap(), None);
        assert_eq!(parse_forever_upload(&with(&format!("{}, items = {{ [1] = \"1,1,1,;0x1\" }}", base.replace("realm = \"R\"", "realm = \" \"")))).unwrap(), None);
        // An item string past 256 characters and a non-numeric key are dropped alone; a faction the
        // site does not know is left out rather than sent.
        let up = parse_forever_upload(&with(&format!(
            "{base}, faction = \"Scourge\", items = {{ [1] = \"1,1,1,;0x1\", [2] = \"{}\", x = \"1,1,1,;0x1\" }}",
            "1".repeat(300)
        )))
        .unwrap()
        .unwrap();
        assert_eq!(up.fold.items.keys().collect::<Vec<_>>(), vec!["1"]);
        assert_eq!(up.fold.faction, None);
    }

    // fix round 1 M5: mirror the server's own bounds, so the companion never sends what it
    // would refuse anyway — `at` non-positive or more than ten years from now.
    #[test]
    fn a_fold_timestamped_far_from_now_is_not_sent() {
        let with = |fold: &str| format!(r#"GoldCapDB = {{ client = {{ interface = 16001, build = "1.60.1.70009", regionId = 90 }}, foreverScan = {{ fold = {{ {fold} }} }} }}"#);
        let base = r#"v = 2, source = "replicate", itemCount = 1, region = 90, realm = "R", build = "1.60.1.70009", interface = 16001, items = { [1] = "1,1,1,;0x1" }"#;
        let now = crate::luafile::now_unix();
        let ten_years = 10 * 365 * 24 * 3600;
        assert_eq!(parse_forever_upload(&with(&format!("{base}, at = {}", now + ten_years + 3600))).unwrap(), None, "forged clock, far future");
        assert_eq!(parse_forever_upload(&with(&format!("{base}, at = 0"))).unwrap(), None, "non-positive at");
        assert!(parse_forever_upload(&with(&format!("{base}, at = {now}"))).unwrap().is_some(), "within the window");
    }

    // fix round 1 M5: an id above Postgres int4 max can never exist in the catalogue — the
    // server drops it as unknown rather than let the lookup query crash, so the companion drops
    // it locally too, alone, the same as an over-long item string.
    #[test]
    fn an_item_id_past_postgres_int4_is_dropped_not_the_whole_fold() {
        let with = |fold: &str| format!(r#"GoldCapDB = {{ client = {{ interface = 16001, build = "1.60.1.70009", regionId = 90 }}, foreverScan = {{ fold = {{ {fold} }} }} }}"#);
        let base = r#"v = 2, at = 1790464249, source = "replicate", itemCount = 1, region = 90, realm = "R", build = "1.60.1.70009", interface = 16001"#;
        let up = parse_forever_upload(&with(&format!(
            "{base}, items = {{ [1] = \"1,1,1,;0x1\", [2147483648] = \"1,1,1,;0x1\" }}"
        )))
        .unwrap()
        .unwrap();
        assert_eq!(up.fold.items.keys().collect::<Vec<_>>(), vec!["1"]);
    }

    // fix round 1 M5: a control character in `ruleset` used to reach an INSERT and crash it
    // server-side (400 on the whole fold); dropping just the ruleset keeps the fold sendable.
    #[test]
    fn a_ruleset_with_a_control_character_is_dropped_alone() {
        let with = |fold: &str| format!(r#"GoldCapDB = {{ client = {{ interface = 16001, build = "1.60.1.70009", regionId = 90 }}, foreverScan = {{ fold = {{ {fold} }} }} }}"#);
        let base = r#"v = 2, at = 1790464249, source = "replicate", itemCount = 1, region = 90, realm = "R", build = "1.60.1.70009", interface = 16001, items = { [1] = "1,1,1,;0x1" }"#;
        let up = parse_forever_upload(&with(&format!("{base}, ruleset = \"a\\1b\""))).unwrap().unwrap();
        assert_eq!(up.fold.ruleset, None);
    }

    // fix round 1 M5: the server counts `ruleset`'s length in characters, not bytes — a byte
    // check would drop a valid non-ASCII ruleset the server would accept.
    #[test]
    fn ruleset_length_is_counted_in_characters_not_bytes() {
        let with = |fold: &str| format!(r#"GoldCapDB = {{ client = {{ interface = 16001, build = "1.60.1.70009", regionId = 90 }}, foreverScan = {{ fold = {{ {fold} }} }} }}"#);
        let base = r#"v = 2, at = 1790464249, source = "replicate", itemCount = 1, region = 90, realm = "R", build = "1.60.1.70009", interface = 16001, items = { [1] = "1,1,1,;0x1" }"#;
        // 32 two-byte Cyrillic characters: 64 bytes in UTF-8, but 32 characters.
        let ruleset = "ы".repeat(32);
        assert_eq!(ruleset.chars().count(), 32);
        assert_eq!(ruleset.len(), 64);
        let up = parse_forever_upload(&with(&format!("{base}, ruleset = \"{ruleset}\""))).unwrap().unwrap();
        assert_eq!(up.fold.ruleset.as_deref(), Some(ruleset.as_str()));
    }

    /// One canned HTTP answer on a local port; hands back the base URL and the raw request.
    pub(crate) fn serve_once(response: String) -> (String, std::sync::mpsc::Receiver<String>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let mut buf = [0u8; 65536];
            // Read headers, then as much body as Content-Length says.
            loop {
                let n = stream.read(&mut buf).unwrap();
                if n == 0 { break; }
                request.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&request);
                if let Some(end) = text.find("\r\n\r\n") {
                    let len = text[..end].lines()
                        .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap_or(0)))
                        .unwrap_or(0);
                    if request.len() >= end + 4 + len { break; }
                }
            }
            tx.send(String::from_utf8_lossy(&request).into_owned()).unwrap();
            let _ = stream.write_all(response.as_bytes());
        });
        (base, rx)
    }

    /// Several canned answers on one local port, served to consecutive connections in order.
    pub(crate) fn serve_forever(responses: Vec<String>) -> (String, std::sync::mpsc::Receiver<String>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for response in responses {
                let Ok((mut stream, _)) = listener.accept() else { return };
                let mut request = Vec::new();
                let mut buf = [0u8; 65536];
                loop {
                    let n = stream.read(&mut buf).unwrap_or(0);
                    if n == 0 { break; }
                    request.extend_from_slice(&buf[..n]);
                    let text = String::from_utf8_lossy(&request);
                    if let Some(end) = text.find("\r\n\r\n") {
                        let len = text[..end].lines()
                            .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap_or(0)))
                            .unwrap_or(0);
                        if request.len() >= end + 4 + len { break; }
                    }
                }
                let _ = tx.send(String::from_utf8_lossy(&request).into_owned());
                let _ = stream.write_all(response.as_bytes());
            }
        });
        (base, rx)
    }

    pub(crate) fn answer(status: &str, body: &str) -> String {
        format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
    }

    #[tokio::test]
    async fn the_upload_names_the_forever_passport_and_the_token() {
        let up = parse_forever_upload(REAL).unwrap().unwrap();
        let (base, seen) = serve_once(answer("200 OK", r#"{"status":"accepted","market":"us-beta-classic-beta-pve-2-horde","items":1974,"dropped":0}"#));
        let out = upload_fold(&crate::sync::build_client(), &base, "tok", &up).await;
        assert_eq!(out, UploadOutcome::Done { market: Some("us-beta-classic-beta-pve-2-horde".into()), note: None, impact: None });
        let request = seen.recv().unwrap();
        let lower = request.to_ascii_lowercase();
        assert!(lower.starts_with("post /v1/forever/scans "), "{}", &request[..60]);
        assert!(lower.contains("x-goldcap-client: 16001/1.60.1.70009"));
        assert!(lower.contains("authorization: bearer tok"));
        assert!(request.contains("\"realm\":\"Classic Beta PvE 2\""));
        assert!(!request.contains("fixture-sale-1"), "the ledger never rides along");
    }

    #[tokio::test]
    async fn what_the_site_says_decides_whether_the_fold_is_done() {
        let up = parse_forever_upload(REAL).unwrap().unwrap();
        let cases = [
            ("200 OK", r#"{"status":"quarantined","market":"m","items":1,"dropped":0,"reason":"unlinked"}"#, true),
            ("200 OK", r#"{"status":"duplicate","market":"m","items":1,"dropped":0}"#, true),
            ("422 Unprocessable Entity", r#"{"status":"rejected","reason":"stale","market":"m"}"#, true),
            ("409 Conflict", r#"{"error":"wrong_game"}"#, true),
            ("400 Bad Request", r#"{"error":"bad_request"}"#, true),
            ("413 Payload Too Large", r#"{"error":"too_large"}"#, true),
            ("401 Unauthorized", r#"{"error":"unauthorized"}"#, false),
            ("429 Too Many Requests", r#"{"error":"rate_limited","retryAfterSec":60}"#, false),
            ("502 Bad Gateway", "<html>", false),
        ];
        for (status, body, done) in cases {
            let (base, _seen) = serve_once(answer(status, body));
            let out = upload_fold(&crate::sync::build_client(), &base, "tok", &up).await;
            assert_eq!(matches!(out, UploadOutcome::Done { .. }), done, "{status} {body}");
        }
        // Nobody listening at all: retried.
        let out = upload_fold(&crate::sync::build_client(), "http://127.0.0.1:9", "tok", &up).await;
        assert!(matches!(out, UploadOutcome::Retry(_)));
    }

    #[tokio::test]
    async fn an_unlinked_account_is_told_what_to_do() {
        let up = parse_forever_upload(REAL).unwrap().unwrap();
        let (base, _seen) = serve_once(answer("200 OK", r#"{"status":"quarantined","market":"m","items":1,"dropped":0,"reason":"unlinked"}"#));
        let UploadOutcome::Done { note, .. } = upload_fold(&crate::sync::build_client(), &base, "tok", &up).await else { panic!() };
        assert_eq!(note.as_deref(), Some("Link Battle.net on goldcap.gg for your scans to count in public prices"));
    }

    fn impact_of(status: &str, impact: &str) -> String {
        format!(r#"{{"status":"{status}","market":"m","items":1,"dropped":0{impact}}}"#)
    }

    #[tokio::test]
    async fn an_answer_carries_the_scan_impact_only_when_it_is_whole_and_counted() {
        let up = parse_forever_upload(REAL).unwrap().unwrap();
        let good = r#","impact":{"updated":412,"onlyYours":38,"firstOnMarket":false}"#;
        let cases = vec![
            (impact_of("accepted", good), Some((412, 38, false))),
            // A re-sent fold (duplicate) hands back the numbers of the first answer.
            (impact_of("duplicate", good), Some((412, 38, false))),
            (impact_of("accepted", r#","impact":{"updated":2210,"onlyYours":2210,"firstOnMarket":true}"#), Some((2210, 2210, true))),
            (impact_of("accepted", r#","impact":{"updated":0,"onlyYours":0,"firstOnMarket":false}"#), Some((0, 0, false))),
            // An old site sends none.
            (impact_of("accepted", ""), None),
            // A quarantined answer is never a number, even if the body carried one.
            (impact_of("quarantined", good), None),
            // Malformed: dropped whole, never a wrong number.
            (impact_of("accepted", r#","impact":{"updated":-1,"onlyYours":0,"firstOnMarket":false}"#), None),
            (impact_of("accepted", r#","impact":{"updated":"412","onlyYours":38,"firstOnMarket":false}"#), None),
            (impact_of("accepted", r#","impact":{"updated":412.5,"onlyYours":38,"firstOnMarket":false}"#), None),
            (impact_of("accepted", r#","impact":{"updated":4294967296,"onlyYours":38,"firstOnMarket":false}"#), None),
            (impact_of("accepted", r#","impact":{"updated":10,"onlyYours":11,"firstOnMarket":false}"#), None),
            (impact_of("accepted", r#","impact":{"updated":10,"onlyYours":5}"#), None),
            (impact_of("accepted", r#","impact":{"updated":10,"onlyYours":5,"firstOnMarket":"yes"}"#), None),
            (impact_of("accepted", r#","impact":{"onlyYours":5,"firstOnMarket":false}"#), None),
            (impact_of("accepted", r#","impact":null"#), None),
            (impact_of("accepted", r#","impact":[412,38,false]"#), None),
        ];
        for (body, want) in cases {
            let (base, _seen) = serve_once(answer("200 OK", &body));
            let UploadOutcome::Done { impact, .. } = upload_fold(&crate::sync::build_client(), &base, "tok", &up).await else { panic!("{body}") };
            assert_eq!(impact, want, "{body}");
        }
    }

    // fix round 1 M4: a captive portal or a proxy's own error page can answer 200 with no
    // `status` field at all — that is not the site having its say, so the fold must be retried.
    #[tokio::test]
    async fn a_200_without_a_json_status_is_retried_not_sent() {
        let up = parse_forever_upload(REAL).unwrap().unwrap();
        let (base, _seen) = serve_once(answer("200 OK", r#"{"ok":true}"#));
        let out = upload_fold(&crate::sync::build_client(), &base, "tok", &up).await;
        assert!(matches!(out, UploadOutcome::Retry(_)), "{out:?}");
    }

    // fix round 1 M7: only accepted/duplicate name the install's market — E6 wants the latest
    // *accepted* fold's market, and a quarantined or rejected answer can name another one (a
    // stale fold from another character's realm) that must not switch it.
    #[tokio::test]
    async fn the_market_is_taken_only_from_accepted_or_duplicate_answers() {
        let up = parse_forever_upload(REAL).unwrap().unwrap();
        let (base, _seen) = serve_once(answer("200 OK", r#"{"status":"quarantined","market":"m","items":1,"dropped":0,"reason":"unlinked"}"#));
        let UploadOutcome::Done { market, .. } = upload_fold(&crate::sync::build_client(), &base, "tok", &up).await else { panic!() };
        assert_eq!(market, None);

        let (base, _seen) = serve_once(answer("200 OK", r#"{"status":"duplicate","market":"m","items":1,"dropped":0}"#));
        let UploadOutcome::Done { market, .. } = upload_fold(&crate::sync::build_client(), &base, "tok", &up).await else { panic!() };
        assert_eq!(market.as_deref(), Some("m"));
    }

    // fix round 1 M8: distinct from a generic Retry, so the caller can stop promising "shared on
    // your next /reload" while the token itself is bad.
    #[tokio::test]
    async fn a_401_is_unauthorized_not_a_generic_retry() {
        let up = parse_forever_upload(REAL).unwrap().unwrap();
        let (base, _seen) = serve_once(answer("401 Unauthorized", r#"{"error":"unauthorized"}"#));
        let out = upload_fold(&crate::sync::build_client(), &base, "tok", &up).await;
        assert_eq!(out, UploadOutcome::Unauthorized);
    }

    // fix round 1 M8, end to end: a revoked token 401s, and until a later attempt actually
    // succeeds the Forever install's AppData must not promise sharing that is not happening.
    #[tokio::test]
    async fn foreverupload_stays_false_after_a_401_until_the_next_success() {
        let root = machine("401-then-ok");
        put(&root, "_classic_beta_", REAL);
        let config = crate::config::Config { companion_token: "tok".into(), ..crate::config::Config::default() };
        let logger = crate::logging::Logger::new(&root.join("logs")).unwrap();
        let store = std::sync::Mutex::new(std::collections::HashMap::new());
        let state_path = root.join("s.json");

        let (base, _seen) = serve_once(answer("401 Unauthorized", r#"{"error":"unauthorized"}"#));
        let state = sync_forever_at_root(&crate::sync::build_client(), &base, &root, &config, &logger, &state_path, &store, 1).await;
        assert!(state.installs.values().next().unwrap().unauthorized);
        let lua = std::fs::read_to_string(root.join("_classic_beta_/Interface/AddOns/GoldCap_AppData/AppData.lua")).unwrap();
        assert!(!lua.contains("foreverUpload"), "{lua}");

        let (base, _seen) = serve_once(answer("200 OK", r#"{"status":"accepted","market":"m","items":1974,"dropped":0}"#));
        let state = sync_forever_at_root(&crate::sync::build_client(), &base, &root, &config, &logger, &state_path, &store, 2).await;
        assert!(!state.installs.values().next().unwrap().unauthorized);
        let lua = std::fs::read_to_string(root.join("_classic_beta_/Interface/AddOns/GoldCap_AppData/AppData.lua")).unwrap();
        assert!(lua.contains("foreverUpload = true"), "{lua}");

        std::fs::remove_dir_all(&root).ok();
    }

    // fix round 1 M5: the server refuses a body over MAX_UPLOAD_BYTES whole (413) anyway, but on
    // a slow uplink the client's own request timeout fires first, turning it into an endless
    // Retry. The pre-check must catch it before ever sending.
    #[tokio::test]
    async fn a_body_over_two_megabytes_is_marked_done_without_being_sent() {
        let root = machine("oversized");
        let mut entries = String::new();
        for i in 1..=9000 {
            entries.push_str(&format!("[{i}] = \"{}\",", "1".repeat(MAX_ITEM_CHARS)));
        }
        let lua = format!(
            r#"GoldCapDB = {{ client = {{ interface = 16001, build = "1.60.1.70009", regionId = 90 }}, foreverScan = {{ fold = {{ v = 2, at = 1790464249, source = "replicate", itemCount = 9000, region = 90, realm = "R", build = "1.60.1.70009", interface = 16001, items = {{ {entries} }} }} }} }}"#
        );
        put(&root, "_classic_beta_", &lua);
        let config = crate::config::Config { companion_token: "tok".into(), ..crate::config::Config::default() };
        let logger = crate::logging::Logger::new(&root.join("logs")).unwrap();
        let store = std::sync::Mutex::new(std::collections::HashMap::new());
        let state_path = root.join("s.json");
        // Port 9 answers nothing at all — if the pre-check did not trip, this would be a Retry.
        let state = sync_forever_at_root(&crate::sync::build_client(), "http://127.0.0.1:9", &root, &config, &logger, &state_path, &store, 1).await;
        assert_eq!(state.sent.len(), 1, "an oversized fold must be marked done locally, never endlessly retried");
        assert_eq!(
            state.installs.values().next().unwrap().note.as_deref(),
            Some("The last scan was too large to send")
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn state_round_trips_and_a_missing_or_broken_file_is_empty() {
        let dir = std::env::temp_dir().join(format!("goldcap-forever-state-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join(STATE_FILE_NAME);
        assert_eq!(ForeverState::load_from(&path), ForeverState::default());
        let mut s = ForeverState::default();
        s.sent.insert("/x/GoldCap.lua".into(), 1790464249);
        s.installs.entry("/x".into()).or_default().market = Some("us-beta-classic-beta-pve-2-horde".into());
        s.save_to(&path).unwrap();
        assert_eq!(ForeverState::load_from(&path), s);
        std::fs::write(&path, "{nope").unwrap();
        assert_eq!(ForeverState::load_from(&path), ForeverState::default());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_gcf1_body_is_kept_only_for_its_own_market() {
        let body = "GCF1;us-beta-x-horde;90;Classic Beta PvE 2;Horde;1790000000;I:2589=68=70=69=79=7000=2=0,2592=255=269===2029=1=20";
        assert_eq!(summarize_gcf1(body, "us-beta-x-horde"), Ok(CrowdSummary { items: 2, ts: 1_790_000_000 }));
        assert!(summarize_gcf1(body, "us-beta-y-horde").is_err(), "another market");
        assert!(summarize_gcf1("GCM1;eu;1;I:1=2", "us-beta-x-horde").is_err(), "retail's payload");
        assert!(summarize_gcf1("GCF1;us-beta-x-horde;90;R;-;1;I:", "us-beta-x-horde").is_err(), "no items");
        assert!(summarize_gcf1("no_data", "us-beta-x-horde").is_err());
    }

    #[tokio::test]
    async fn prices_come_back_revalidated_and_stop_being_written_when_three_days_old() {
        let store = std::sync::Mutex::new(std::collections::HashMap::new());
        let logger = crate::logging::Logger::new(&std::env::temp_dir().join(format!("goldcap-forever-log-{}", std::process::id()))).unwrap();
        let body = "GCF1;m;90;R;Horde;1790000000;I:1=2=2===3=1=0";
        let (base, _seen) = serve_once(format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nETag: W/\"gcf1-m-1790000100\"\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        ));
        let got = refresh_crowd_at(&crate::sync::build_client(), &base, &store, "/wow/_classic_beta_", "m", &logger, 1_790_000_600).await;
        assert_eq!(got, Some((body.to_string(), CrowdSummary { items: 1, ts: 1_790_000_000 })));

        let (base, seen) = serve_once("HTTP/1.1 304 Not Modified\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into());
        let again = refresh_crowd_at(&crate::sync::build_client(), &base, &store, "/wow/_classic_beta_", "m", &logger, 1_790_000_900).await;
        assert!(again.is_some());
        let request = seen.recv().unwrap().to_ascii_lowercase();
        assert!(request.contains("if-none-match: w/\"gcf1-m-1790000100\""), "{request}");
        assert!(request.contains("market=m"));

        // Three days on and the site unreachable: the kept body is no longer written.
        let late = refresh_crowd_at(&crate::sync::build_client(), "http://127.0.0.1:9", &store, "/wow/_classic_beta_", "m", &logger, 1_790_000_000 + CROWD_MAX_AGE_SECS + 1).await;
        assert_eq!(late, None);
    }

    fn machine(label: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("goldcap-forever-machine-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
    fn put(root: &std::path::Path, folder: &str, lua: &str) -> std::path::PathBuf {
        let sv = root.join(folder).join("WTF").join("Account").join("A").join("SavedVariables");
        std::fs::create_dir_all(&sv).unwrap();
        std::fs::write(sv.join("GoldCap.lua"), lua).unwrap();
        sv.join("GoldCap.lua")
    }
    fn files_under(dir: &std::path::Path) -> Vec<String> {
        let mut out = Vec::new();
        for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            if e.path().is_dir() { out.extend(files_under(&e.path())); } else { out.push(e.path().to_string_lossy().into_owned()); }
        }
        out.sort();
        out
    }

    #[tokio::test]
    async fn a_retail_only_machine_gets_no_request_and_no_new_file() {
        let root = machine("retail-only");
        put(&root, "_retail_", r#"GoldCapDB = { client = { interface = 120100, build = "12.1.0.69933" }, ledger = {} }"#);
        let before = files_under(&root);
        let config = crate::config::Config {
            wow_retail_path: root.join("_retail_").to_string_lossy().into_owned(),
            forever_root_path: root.to_string_lossy().into_owned(),
            companion_token: "tok".into(),
            ..crate::config::Config::default()
        };
        let logger = crate::logging::Logger::new(&root.join("logs")).unwrap();
        let store = std::sync::Mutex::new(std::collections::HashMap::new());
        // Port 9 answers nothing: any request would be a Retry logged; none must be made at all.
        let state = sync_forever_at(&crate::sync::build_client(), "http://127.0.0.1:9", &config, &logger, &root.join("state").join(STATE_FILE_NAME), &store, 1_790_000_000).await;
        assert_eq!(state.sent.len(), 0);
        // No Forever install ever seen: no games list either, exactly like 1.13.0 (fix round 1 I1).
        assert!(state.games.is_empty(), "{:?}", state.games);
        let after: Vec<String> = files_under(&root).into_iter().filter(|p| !p.contains("/logs/")).collect();
        assert_eq!(after, before, "nothing new under the WoW root, including no forever.json");
        assert!(!root.join("_retail_/Interface/AddOns/GoldCap_AppData").exists());
        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn retail_and_forever_side_by_side_only_forever_is_touched() {
        let root = machine("both");
        put(&root, "_retail_", r#"GoldCapDB = { client = { interface = 120100, build = "12.1.0.69933" }, ledger = {} }"#);
        let sv = put(&root, "_classic_beta_", REAL);
        let config = crate::config::Config {
            wow_retail_path: root.join("_retail_").to_string_lossy().into_owned(),
            forever_root_path: root.to_string_lossy().into_owned(),
            companion_token: "tok".into(),
            ..crate::config::Config::default()
        };
        let logger = crate::logging::Logger::new(&root.join("logs")).unwrap();
        let store = std::sync::Mutex::new(std::collections::HashMap::new());
        let (base, seen) = serve_once(answer("200 OK", r#"{"status":"accepted","market":"m","items":1974,"dropped":0}"#));
        let state_path = root.join("state").join(STATE_FILE_NAME);
        let state = sync_forever_at(&crate::sync::build_client(), &base, &config, &logger, &state_path, &store, 1_790_464_500).await;
        assert!(seen.recv().unwrap().to_ascii_lowercase().starts_with("post /v1/forever/scans"));
        assert_eq!(state.sent.get(&sv.to_string_lossy().into_owned()), Some(&1_790_464_249));
        // The Forever install got its own AppData; retail's folder got nothing.
        let forever_dir = root.join("_classic_beta_/Interface/AddOns/GoldCap_AppData");
        assert!(forever_dir.join(crate::luafile::FOREVER_TOC_FILE_NAME).is_file());
        assert!(std::fs::read_to_string(forever_dir.join(crate::luafile::LUA_FILE_NAME)).unwrap().contains("foreverUpload = true"));
        assert!(!root.join("_retail_/Interface/AddOns/GoldCap_AppData").exists());
        // The same fold on the next tick is not sent again (no server is listening now).
        let again = sync_forever_at(&crate::sync::build_client(), "http://127.0.0.1:9", &config, &logger, &state_path, &store, 1_790_464_900).await;
        assert_eq!(again.installs.values().next().unwrap().market.as_deref(), Some("m"));
        std::fs::remove_dir_all(&root).ok();
    }

    // The two-drives case: Retail lives under one WoW folder, Forever's client under a
    // completely separate one (its own `forever_root_path`, standing in for e.g. `D:\Games\World
    // of Warcraft`). sync_forever must scan Forever's own root, never Retail's — a config that
    // only set wow_retail_path (no forever_root_path) must not fall back through it.
    #[tokio::test]
    async fn sync_forever_uses_the_forever_root_when_retail_lives_on_a_different_drive() {
        let retail_drive = machine("two-drives-retail");
        let forever_drive = machine("two-drives-forever");
        put(&retail_drive, "_retail_", r#"GoldCapDB = { client = { interface = 120100, build = "12.1.0.69933" }, ledger = {} }"#);
        let sv = put(&forever_drive, "_classic_beta_", REAL);

        let config = crate::config::Config {
            wow_retail_path: retail_drive.join("_retail_").to_string_lossy().into_owned(),
            forever_root_path: forever_drive.to_string_lossy().into_owned(),
            companion_token: "tok".into(),
            ..crate::config::Config::default()
        };
        let logger = crate::logging::Logger::new(&forever_drive.join("logs")).unwrap();
        let store = std::sync::Mutex::new(std::collections::HashMap::new());
        let (base, seen) = serve_once(answer("200 OK", r#"{"status":"accepted","market":"m","items":1974,"dropped":0}"#));
        let state_path = forever_drive.join("state").join(STATE_FILE_NAME);
        let state = sync_forever_at(&crate::sync::build_client(), &base, &config, &logger, &state_path, &store, 1_790_464_500).await;

        assert!(seen.recv().unwrap().to_ascii_lowercase().starts_with("post /v1/forever/scans"));
        assert_eq!(state.sent.get(&sv.to_string_lossy().into_owned()), Some(&1_790_464_249));
        // Nothing was ever written under the retail drive — it was never scanned at all.
        assert!(!retail_drive.join("_retail_/Interface/AddOns/GoldCap_AppData").exists());

        std::fs::remove_dir_all(&retail_drive).ok();
        std::fs::remove_dir_all(&forever_drive).ok();
    }

    // The Status screen's Forever card reads realm/faction/item count straight off the
    // snapshot rather than re-parsing the market slug in JS — this is where those get filled in.
    #[tokio::test]
    async fn an_accepted_fold_records_realm_faction_and_item_count_for_the_status_card() {
        let root = machine("realm-faction");
        put(&root, "_classic_beta_", REAL);
        let config = crate::config::Config { companion_token: "tok".into(), ..crate::config::Config::default() };
        let logger = crate::logging::Logger::new(&root.join("logs")).unwrap();
        let store = std::sync::Mutex::new(std::collections::HashMap::new());
        let (base, _seen) = serve_once(answer("200 OK", r#"{"status":"accepted","market":"m","items":1974,"dropped":0}"#));
        let state = sync_forever_at_root(&crate::sync::build_client(), &base, &root, &config, &logger, &root.join("s.json"), &store, 1).await;
        let entry = state.installs.values().next().unwrap();
        assert_eq!(entry.realm.as_deref(), Some("Classic Beta PvE 2"));
        assert_eq!(entry.faction.as_deref(), Some("Horde"));
        assert_eq!(entry.sent_items, Some(1974));
        std::fs::remove_dir_all(&root).ok();
    }

    // fix round 1 M7's rule extended to realm/faction: a quarantined answer must not overwrite
    // a good fold's realm with a stale one's.
    #[tokio::test]
    async fn a_quarantined_fold_does_not_overwrite_the_realm_or_faction() {
        let up = parse_forever_upload(REAL).unwrap().unwrap();
        let (base, _seen) = serve_once(answer("200 OK", r#"{"status":"quarantined","market":"m","items":1,"dropped":0,"reason":"unlinked"}"#));
        let UploadOutcome::Done { market, .. } = upload_fold(&crate::sync::build_client(), &base, "tok", &up).await else { panic!() };
        assert_eq!(market, None, "quarantined never sets a market, so nothing overwrites realm/faction either");
    }

    // "written to the addon" is local write time, distinct from crowd_ts (the site's own
    // snapshot age) — this is what the Status screen's "crowd prices written ... <age>" reads.
    #[tokio::test]
    async fn crowd_written_at_is_set_when_the_addon_file_actually_carries_prices() {
        let root = machine("crowd-written");
        put(&root, "_classic_beta_", REAL);
        let config = crate::config::Config { companion_token: "tok".into(), ..crate::config::Config::default() };
        let logger = crate::logging::Logger::new(&root.join("logs")).unwrap();
        let store = std::sync::Mutex::new(std::collections::HashMap::new());

        // First tick: the scan upload succeeds and returns a market, but no crowd payload is
        // ever requested from a server the test never serves a second answer from — the
        // sequential serve_once server only answers the scan POST, so refresh_crowd_at's GET
        // fails and nothing is kept: crowd_written_at must stay unset.
        let (base, _seen) = serve_once(answer("200 OK", r#"{"status":"accepted","market":"m","items":1,"dropped":0}"#));
        let state = sync_forever_at_root(&crate::sync::build_client(), &base, &root, &config, &logger, &root.join("s.json"), &store, 1).await;
        let entry = state.installs.values().next().unwrap();
        assert_eq!(entry.crowd_written_at, None, "no crowd payload was ever fetched");
        std::fs::remove_dir_all(&root).ok();
    }

    // The answer's numbers reach the Status card (installs[dir].impact) and, per SavedVariables
    // file, the addon (impacts[file] -> `foreverImpact` in AppData.lua). A later answer without
    // numbers (an old site) clears both, so nothing older sits under a newer "Scan uploaded".
    #[tokio::test]
    async fn the_scan_impact_reaches_the_card_and_the_addon_and_the_next_answer_replaces_it() {
        let root = machine("impact");
        let sv = put(&root, "_classic_beta_", REAL);
        let config = crate::config::Config { companion_token: "tok".into(), ..crate::config::Config::default() };
        let logger = crate::logging::Logger::new(&root.join("logs")).unwrap();
        let store = std::sync::Mutex::new(std::collections::HashMap::new());
        let state_path = root.join("s.json");
        let lua_path = root.join("_classic_beta_/Interface/AddOns/GoldCap_AppData/AppData.lua");
        let file_key = sv.to_string_lossy().into_owned();

        let (base, _seen) = serve_once(answer("200 OK", &impact_of("accepted", r#","impact":{"updated":412,"onlyYours":38,"firstOnMarket":false}"#)));
        let state = sync_forever_at_root(&crate::sync::build_client(), &base, &root, &config, &logger, &state_path, &store, 1_790_464_500).await;
        let want = ScanImpact {
            at: 1_790_464_249,
            sent_at: 1_790_464_500,
            updated: 412,
            only_yours: 38,
            first: false,
            realm: "Classic Beta PvE 2".into(),
            faction: Some("Horde".into()),
        };
        assert_eq!(state.impacts.get(&file_key), Some(&want));
        assert_eq!(state.installs.values().next().unwrap().impact.as_ref(), Some(&want));
        assert_eq!(state.games[0].state.impact.as_ref(), Some(&want), "the Status screen reads it off the game row");
        assert_eq!(
            std::fs::read_to_string(&lua_path).unwrap(),
            "GoldCap_AppData = { foreverUpload = true, foreverImpact = { { at = 1790464249, updated = 412, onlyYours = 38, realm = 'Classic Beta PvE 2', faction = 'Horde' } }, writtenAt = 1790464500 }\n"
        );
        // It is kept on disk: a restart still has it.
        assert_eq!(ForeverState::load_from(&state_path).impacts.get(&file_key), Some(&want));

        // A new fold, answered by a site that sends no numbers: both are cleared.
        let newer = std::fs::read_to_string(&sv).unwrap().replace("[\"at\"] = 1790464249,", "[\"at\"] = 1790464300,") + "\n-- saved again\n";
        std::fs::write(&sv, newer).unwrap();
        let (base, _seen) = serve_once(answer("200 OK", &impact_of("accepted", "")));
        let state = sync_forever_at_root(&crate::sync::build_client(), &base, &root, &config, &logger, &state_path, &store, 1_790_464_800).await;
        assert_eq!(state.sent.get(&file_key), Some(&1_790_464_300));
        assert!(state.impacts.is_empty());
        assert_eq!(state.installs.values().next().unwrap().impact, None);
        assert_eq!(
            std::fs::read_to_string(&lua_path).unwrap(),
            "GoldCap_AppData = { foreverUpload = true, writtenAt = 1790464800 }\n",
            "no numbers, no key: the bytes 1.15.0 wrote"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    // One install can hold two WoW accounts: each SavedVariables file keeps its own result, and
    // both go to the addon (which prints only the one whose fold it holds).
    #[tokio::test]
    async fn two_accounts_keep_one_result_each() {
        let root = machine("impact-two");
        let a = put(&root, "_classic_beta_", REAL);
        let sv_b = root.join("_classic_beta_/WTF/Account/B/SavedVariables");
        std::fs::create_dir_all(&sv_b).unwrap();
        std::fs::write(sv_b.join("GoldCap.lua"), std::fs::read_to_string(&a).unwrap().replace("[\"at\"] = 1790464249,", "[\"at\"] = 1790464300,")).unwrap();
        let config = crate::config::Config { companion_token: "tok".into(), ..crate::config::Config::default() };
        let logger = crate::logging::Logger::new(&root.join("logs")).unwrap();
        let store = std::sync::Mutex::new(std::collections::HashMap::new());
        let (base, _seen) = serve_forever(vec![
            answer("200 OK", &impact_of("accepted", r#","impact":{"updated":10,"onlyYours":1,"firstOnMarket":false}"#)),
            answer("200 OK", &impact_of("accepted", r#","impact":{"updated":20,"onlyYours":20,"firstOnMarket":true}"#)),
        ]);
        let state = sync_forever_at_root(&crate::sync::build_client(), &base, &root, &config, &logger, &root.join("s.json"), &store, 1_790_464_500).await;
        assert_eq!(state.impacts.len(), 2);
        let lua = std::fs::read_to_string(root.join("_classic_beta_/Interface/AddOns/GoldCap_AppData/AppData.lua")).unwrap();
        assert_eq!(lua.matches("{ at = ").count(), 2, "{lua}");
        std::fs::remove_dir_all(&root).ok();
    }

    // A forever.json from 1.15.0 (no impact keys) loads into 1.16.0, and a 1.16.0 one loads into
    // 1.15.0's shape too: unknown keys were always ignored (the structs do not deny them).
    #[test]
    fn forever_json_is_compatible_in_both_directions() {
        let old = r#"{"sent":{"/a":1},"installs":{"/w":{"market":"m","realm":"R","lastSentAt":5,"sentItems":3,"unauthorized":false}},"games":[]}"#;
        let loaded: ForeverState = serde_json::from_str(old).unwrap();
        assert!(loaded.impacts.is_empty());
        assert_eq!(loaded.installs["/w"].impact, None);
        assert_eq!(loaded.installs["/w"].sent_items, Some(3));

        let mut new = loaded.clone();
        let impact = ScanImpact { at: 9, sent_at: 10, updated: 4, only_yours: 2, first: true, realm: "R".into(), faction: None };
        new.impacts.insert("/a".into(), impact.clone());
        new.installs.get_mut("/w").unwrap().impact = Some(impact);
        let text = serde_json::to_string(&new).unwrap();
        assert_eq!(serde_json::from_str::<ForeverState>(&text).unwrap(), new);
        assert!(text.contains(r#""onlyYours":2"#) && text.contains(r#""sentAt":10"#), "{text}");
    }

    #[tokio::test]
    async fn unpaired_nothing_is_uploaded_but_the_install_is_listed() {
        let root = machine("unpaired");
        put(&root, "_classic_beta_", REAL);
        let config = crate::config::Config::default(); // no retail path, no token
        let logger = crate::logging::Logger::new(&root.join("logs")).unwrap();
        let store = std::sync::Mutex::new(std::collections::HashMap::new());
        let state = sync_forever_at_root(&crate::sync::build_client(), "http://127.0.0.1:9", &root, &config, &logger, &root.join("s.json"), &store, 1).await;
        assert!(state.sent.is_empty());
        assert_eq!(state.games[0].game, crate::games::GameKind::Forever);
        assert_eq!(state.games[0].state.last_scan_at, Some(1_790_464_249));
        let lua = std::fs::read_to_string(root.join("_classic_beta_/Interface/AddOns/GoldCap_AppData/AppData.lua")).unwrap();
        assert_eq!(lua, "GoldCap_AppData = { writtenAt = 1 }\n", "no foreverUpload: the addon must not promise sharing");
        std::fs::remove_dir_all(&root).ok();
    }

    /// Run by hand against plan 2a's local stand (Task 12: API on :8093, token
    /// `stand-forever-token`), never in CI:
    ///   GOLDCAP_E2E_API=http://localhost:8093 cargo test --locked -- --ignored forever_e2e
    /// Reads the owner's real beta SavedVariables, read-only, re-stamps the fold's `at` to now
    /// (the site refuses a fold older than a day) and sends it.
    #[tokio::test]
    #[ignore]
    async fn forever_e2e_against_the_local_stand() {
        let base = std::env::var("GOLDCAP_E2E_API").expect("GOLDCAP_E2E_API");
        let pattern = "/Applications/World of Warcraft/_classic_beta_/WTF/Account";
        let file = std::fs::read_dir(pattern).unwrap().flatten()
            .map(|a| a.path().join("SavedVariables").join("GoldCap.lua"))
            .find(|p| p.is_file()).expect("a beta GoldCap.lua");
        let mut up = parse_forever_upload(&std::fs::read_to_string(&file).unwrap()).unwrap().expect("a Forever fold");
        up.fold.at = crate::luafile::now_unix() - 60;
        let out = upload_fold(&crate::sync::build_client(), &base, "stand-forever-token", &up).await;
        let UploadOutcome::Done { market: Some(market), .. } = out else { panic!("{out:?}") };
        assert_eq!(market, "us-beta-classic-beta-pve-2-horde");
        let logger = crate::logging::Logger::new(&std::env::temp_dir().join("goldcap-e2e-log")).unwrap();
        let store = std::sync::Mutex::new(std::collections::HashMap::new());
        // The stand aggregates on `pnpm --filter @wowa/api forever:aggregate`; run it first.
        let crowd = refresh_crowd_at(&crate::sync::build_client(), &base, &store, "e2e", &market, &logger, crate::luafile::now_unix()).await;
        let (body, summary) = crowd.expect("GCF1 back from the stand — did you run forever:aggregate?");
        assert!(body.starts_with("GCF1;us-beta-classic-beta-pve-2-horde;90;Classic Beta PvE 2;Horde;"));
        assert!(summary.items > 1000);
    }

    #[tokio::test]
    async fn a_paired_forever_install_gets_its_runs_beside_appdata() {
        let root = machine("forever-runs");
        put(&root, "_classic_beta_", REAL);
        let config = crate::config::Config { companion_token: "tok".into(), ..crate::config::Config::default() };
        let logger = crate::logging::Logger::new(&root.join("logs")).unwrap();
        let store = std::sync::Mutex::new(std::collections::HashMap::new());
        let runs = r#"{"v":3,"generatedAt":"2026-10-01T10:00:00.000Z","plan":"pro","freeLines":5,"game":"forever","runs":[{"code":"abcd2345","name":"Tailoring 1 → 100","updatedAt":"2026-10-01T09:00:00.000Z","lines":[{"itemId":2589,"qty":120,"vendor":false,"nameEn":"Linen Cloth","usual":70}]}]}"#;
        let (base, seen) = serve_forever(vec![
            answer("200 OK", r#"{"status":"accepted","market":"us-beta-classic-beta-pve-2-horde","items":1974,"dropped":0}"#),
            answer("500 Internal Server Error", "{}"), // the crowd prices: not this test's business
            answer("200 OK", runs),
        ]);
        sync_forever_at_root(&crate::sync::build_client(), &base, &root, &config, &logger, &root.join("s.json"), &store, 1).await;
        let requests: Vec<String> = seen.try_iter().collect();
        assert!(requests.iter().any(|r| r.starts_with("GET /v1/lists/companion?game=forever&market=us-beta-classic-beta-pve-2-horde ")), "{requests:?}");
        let dir = root.join("_classic_beta_/Interface/AddOns/GoldCap_AppData");
        let written = std::fs::read_to_string(dir.join(crate::luafile::RUNS_FILE_NAME)).unwrap();
        assert!(written.contains("{ i = 2589, q = 120, v = false, n = 'Linen Cloth', u = 70 }"), "{written}");
        assert!(std::fs::read_to_string(dir.join(crate::luafile::FOREVER_TOC_FILE_NAME)).unwrap().contains("\nRuns.lua\n"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn unpaired_no_runs_are_asked_for_or_written() {
        let root = machine("forever-runs-unpaired");
        put(&root, "_classic_beta_", REAL);
        let config = crate::config::Config::default();
        let logger = crate::logging::Logger::new(&root.join("logs")).unwrap();
        let store = std::sync::Mutex::new(std::collections::HashMap::new());
        sync_forever_at_root(&crate::sync::build_client(), "http://127.0.0.1:9", &root, &config, &logger, &root.join("s.json"), &store, 1).await;
        assert!(!root.join("_classic_beta_/Interface/AddOns/GoldCap_AppData").join(crate::luafile::RUNS_FILE_NAME).exists());
        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn a_paired_forever_install_sends_its_buy_purchases_once() {
        let root = machine("forever-buys");
        let sv = put(&root, "_classic_beta_", r#"GoldCapDB = { client = { interface = 16001, build = "1.60.1.70009", regionId = 90 }, ledger = {
          { key = "buyrun-a", kind = "buy", source = "goldcap_buy", itemID = 2589, qty = 53, total = 3551, runCode = "k7f3qzab", at = 1790000000 } } }"#);
        let config = crate::config::Config { companion_token: "tok".into(), ..crate::config::Config::default() };
        let logger = crate::logging::Logger::new(&root.join("logs")).unwrap();
        let store = std::sync::Mutex::new(std::collections::HashMap::new());
        let (base, seen) = serve_forever(vec![
            answer("200 OK", r#"{"accepted":1,"stored":1}"#),
            answer("200 OK", r#"{"v":3,"generatedAt":"2026-10-01T10:00:00.000Z","plan":"free","freeLines":5,"game":"forever","runs":[]}"#),
        ]);
        let state = sync_forever_at_root(&crate::sync::build_client(), &base, &root, &config, &logger, &root.join("s.json"), &store, 1).await;
        let first: Vec<String> = seen.try_iter().collect();
        assert!(first[0].to_ascii_lowercase().starts_with("post /v1/forever/run-purchases "), "{first:?}");
        let key = sv.to_string_lossy().into_owned();
        assert!(state.purchases_sent.get(&key).is_some_and(|s| s.contains("buyrun-a")));
        assert!(state.purchase_stamps.contains_key(&key));
        // Same file next tick: not re-read, not re-sent (nothing listens on port 9 anyway).
        let again = sync_forever_at_root(&crate::sync::build_client(), "http://127.0.0.1:9", &root, &config, &logger, &root.join("s.json"), &store, 2).await;
        assert!(again.purchases_sent.get(&key).is_some_and(|s| s.contains("buyrun-a")));
        std::fs::remove_dir_all(&root).ok();
    }
}
