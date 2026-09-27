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

fn item_id(key: &Value) -> Option<i64> {
    match key {
        Value::Integer(i) if (1..=9_999_999_999).contains(i) => Some(*i),
        Value::Number(n) if n.fract() == 0.0 && *n >= 1.0 && *n <= 9_999_999_999.0 => Some(*n as i64),
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

/// The Forever upload one SavedVariables file holds, or None: no Forever passport, no fold,
/// or a fold the site would refuse whole (no items, an unknown source, no realm). A courier:
/// the items go as the addon wrote them, and the site checks their grammar.
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
    if items.is_empty() || items.len() > MAX_ITEMS || !SOURCES.contains(&source.as_str()) || realm.is_empty() || realm.chars().count() > 64 {
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
            ruleset: opt_string(&f, "ruleset").map(|r| r.trim().to_string()).filter(|r| !r.is_empty() && r.len() <= 32),
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
    /// `market`: the market the fold was filed under, when the site named one.
    /// `note`: what the Status screen should tell the player, when anything.
    Done { market: Option<String>, note: Option<String> },
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
        Err(e) => return UploadOutcome::Retry(e.to_string()),
    };
    let code = res.status().as_u16();
    let body: serde_json::Value = res.json().await.unwrap_or(serde_json::Value::Null);
    let market = body["market"].as_str().map(str::to_string);
    let status = body["status"].as_str().unwrap_or("");
    match code {
        200 | 422 => UploadOutcome::Done { market, note: note_for(status, body["reason"].as_str()) },
        400 | 409 | 413 => UploadOutcome::Done {
            market: None,
            note: Some(format!("goldcap.gg refused the last scan ({})", body["error"].as_str().unwrap_or("unknown"))),
        },
        _ => UploadOutcome::Retry(format!("status {code}")),
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct InstallState {
    pub market: Option<String>,
    pub last_scan_at: Option<i64>,
    pub last_sent_at: Option<i64>,
    pub note: Option<String>,
    pub crowd_items: Option<u32>,
    pub crowd_ts: Option<i64>,
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
    /// SavedVariables path → the fold `at` the site has had its say about.
    pub sent: BTreeMap<String, i64>,
    /// Install dir → what the Forever leg knows about it.
    pub installs: BTreeMap<String, InstallState>,
    /// Every game folder the last tick found, for the Status screen.
    pub games: Vec<GameStatus>,
}

impl ForeverState {
    pub fn load_from(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn save_to(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, serde_json::to_string_pretty(self).map_err(std::io::Error::from)?)
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
        .query(&[("market", slug)])
        .timeout(std::time::Duration::from_secs(60));
    let fetched = crate::region::fetch_capped(request, etag.as_deref()).await;
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
    match crate::games::wow_root(config) {
        Some(root) => sync_forever_at_root(client, base, &root, config, logger, state_path, store, now).await,
        None => ForeverState::load_from(state_path),
    }
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

    for game in games.iter().filter(|g| g.kind == GameKind::Forever) {
        let key = game.dir.to_string_lossy().into_owned();
        let mut interface = game.interface.unwrap_or(FOREVER_MIN_INTERFACE + 1);
        for file in &game.saved_vars {
            let Ok(src) = std::fs::read_to_string(file) else { continue };
            let up = match parse_forever_upload(&src) {
                Ok(Some(up)) => up,
                Ok(None) => continue,
                Err(e) => {
                    logger.error(&format!("{}: not readable yet ({e})", file.display()));
                    continue;
                }
            };
            interface = up.client.interface;
            let entry = state.installs.entry(key.clone()).or_default();
            entry.last_scan_at = entry.last_scan_at.max(Some(up.fold.at));
            let file_key = file.to_string_lossy().into_owned();
            if token.is_empty() || state.sent.get(&file_key) == Some(&up.fold.at) {
                continue;
            }
            match upload_fold(client, base, &token, &up).await {
                UploadOutcome::Done { market, note } => {
                    logger.info(&format!("forever scan from {}: sent ({} items)", game.folder, up.fold.items.len()));
                    state.sent.insert(file_key, up.fold.at);
                    let entry = state.installs.entry(key.clone()).or_default();
                    entry.last_sent_at = Some(now);
                    entry.note = note;
                    if market.is_some() {
                        entry.market = market;
                    }
                }
                UploadOutcome::Retry(why) => logger.error(&format!("forever scan from {}: will retry ({why})", game.folder)),
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
        if let Err(e) = crate::luafile::write_forever_app_data(
            &crate::luafile::addon_dir(&game.dir),
            interface,
            crowd.as_ref().map(|(body, _)| body.as_str()),
            !token.is_empty(),
            now,
        ) {
            logger.error(&format!("forever prices for {}: could not write ({e})", game.folder));
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
mod tests {
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

    /// One canned HTTP answer on a local port; hands back the base URL and the raw request.
    fn serve_once(response: String) -> (String, std::sync::mpsc::Receiver<String>) {
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

    fn answer(status: &str, body: &str) -> String {
        format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
    }

    #[tokio::test]
    async fn the_upload_names_the_forever_passport_and_the_token() {
        let up = parse_forever_upload(REAL).unwrap().unwrap();
        let (base, seen) = serve_once(answer("200 OK", r#"{"status":"accepted","market":"us-beta-classic-beta-pve-2-horde","items":1974,"dropped":0}"#));
        let out = upload_fold(&crate::sync::build_client(), &base, "tok", &up).await;
        assert_eq!(out, UploadOutcome::Done { market: Some("us-beta-classic-beta-pve-2-horde".into()), note: None });
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
            companion_token: "tok".into(),
            ..crate::config::Config::default()
        };
        let logger = crate::logging::Logger::new(&root.join("logs")).unwrap();
        let store = std::sync::Mutex::new(std::collections::HashMap::new());
        // Port 9 answers nothing: any request would be a Retry logged; none must be made at all.
        let state = sync_forever_at(&crate::sync::build_client(), "http://127.0.0.1:9", &config, &logger, &root.join("state").join(STATE_FILE_NAME), &store, 1_790_000_000).await;
        assert_eq!(state.sent.len(), 0);
        assert_eq!(state.games.iter().map(|g| (g.folder.as_str(), g.game)).collect::<Vec<_>>(), vec![("_retail_", crate::games::GameKind::Retail)]);
        let after: Vec<String> = files_under(&root).into_iter().filter(|p| !p.contains("/logs/") && !p.contains("/state/")).collect();
        assert_eq!(after, before, "nothing new under the WoW root");
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
}
