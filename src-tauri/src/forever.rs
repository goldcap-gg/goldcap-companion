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
// used from Task C5 on
#[allow(dead_code)]
pub const FOREVER_MIN_INTERFACE: i64 = 16_000;
// used from Task C5 on
#[allow(dead_code)]
pub const FOREVER_MAX_INTERFACE: i64 = 16_999;

// used from Task C5 on
#[allow(dead_code)]
pub fn is_forever_interface(interface: i64) -> bool {
    (FOREVER_MIN_INTERFACE..=FOREVER_MAX_INTERFACE).contains(&interface)
}

/// `GoldCapDB.client`, as far as discovery needs it.
// used from Task C5 on
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq)]
pub struct Passport {
    pub interface: i64,
    pub build: String,
    pub region_id: Option<i64>,
}

// used from Task C5 on
#[allow(dead_code)]
fn globals_db(lua: &Lua, source: &str) -> Option<mlua::Table> {
    lua.load(source).exec().ok()?;
    match lua.globals().get::<Value>("GoldCapDB") {
        Ok(Value::Table(db)) => Some(db),
        _ => None,
    }
}

/// The passport a SavedVariables file carries, or None. Anything unreadable is "no passport".
// used from Task C5 on
#[allow(dead_code)]
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
// used from Task C5 on
#[allow(dead_code)]
pub const MAX_ITEMS: usize = 20_000;
// used from Task C5 on
#[allow(dead_code)]
pub const MAX_ITEM_CHARS: usize = 256;
// used from Task C5 on
#[allow(dead_code)]
const SOURCES: [&str; 3] = ["replicate", "replicate+browse", "browse"];
// used from Task C5 on
#[allow(dead_code)]
const FACTIONS: [&str; 3] = ["Horde", "Alliance", "Neutral"];

// used from Task C5 on
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ForeverClient {
    pub interface: i64,
    pub build: String,
    pub region_id: i64,
}

// used from Task C5 on
#[allow(dead_code)]
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
// used from Task C5 on
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ForeverUpload {
    pub client: ForeverClient,
    pub fold: ForeverFold,
}

// used from Task C5 on
#[allow(dead_code)]
fn item_id(key: &Value) -> Option<i64> {
    match key {
        Value::Integer(i) if (1..=9_999_999_999).contains(i) => Some(*i),
        Value::Number(n) if n.fract() == 0.0 && *n >= 1.0 && *n <= 9_999_999_999.0 => Some(*n as i64),
        _ => None,
    }
}

// used from Task C5 on
#[allow(dead_code)]
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
// used from Task C5 on
#[allow(dead_code)]
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

// used from Task C5 on
#[allow(dead_code)]
pub const API_BASE: &str = "https://api.goldcap.gg";
// used from Task C5 on
#[allow(dead_code)]
pub const STATE_FILE_NAME: &str = "forever.json";

// used from Task C5 on
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq)]
pub enum UploadOutcome {
    /// The site has had its say about this fold; it is never sent again (Decision E5).
    /// `market`: the market the fold was filed under, when the site named one.
    /// `note`: what the Status screen should tell the player, when anything.
    Done { market: Option<String>, note: Option<String> },
    /// Nothing decided: the next tick sends it again.
    Retry(String),
}

// used from Task C5 on
#[allow(dead_code)]
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
// used from Task C5 on
#[allow(dead_code)]
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

// used from Task C5 on
#[allow(dead_code)]
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
// used from Task C5 on
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GameStatus {
    pub folder: String,
    pub game: crate::games::GameKind,
    #[serde(flatten)]
    pub state: InstallState,
}

// used from Task C5 on
#[allow(dead_code)]
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

// used from Task C5 on
#[allow(dead_code)]
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
}
