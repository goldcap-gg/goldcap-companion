//! WoW: Forever recipe sources. The addon notes, in the Forever install's
//! `GoldCapDB.foreverSources`, what each profession trainer the player talked to teaches and what
//! each merchant they opened sells; this module sends those to `POST /v1/forever/sources`, so the
//! site can say where a recipe is learned or bought. Nothing about the character goes up: an npc,
//! where it stands, and its list.
//!
//! "Already sent" is per npc: the `at` of the entry the site took. An npc the player visits again
//! gets a new `at` and goes up again; one the file no longer holds is forgotten.

use crate::forever::{item_id, ITEM_MAX_ID};
use crate::forever_runs::PurchasePass;
use crate::savedvars::{flag, opt_exact_int, opt_string, ClientPassport};
use mlua::{Lua, LuaOptions, StdLib, Table, Value};
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::Path;

/// Trainers and merchants together in one request. Must not exceed the site's
/// FOREVER_SOURCES_MAX_NPCS.
pub const MAX_BATCH_NPCS: usize = 25;
/// Rows of one npc. No trainer or merchant comes near it; past it the lowest ids go.
pub const MAX_ROWS: usize = 600;
const MAX_TEXT_CHARS: usize = 64;
const FACTIONS: [&str; 2] = ["Alliance", "Horde"];

/// What both kinds of npc carry, under the site's field names.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Npc {
    pub npc: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub map: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub zone: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subzone: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub faction: Option<String>,
    pub at: i64,
}

/// One thing a trainer teaches.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Service {
    pub spell: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skill: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub level: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item: Option<i64>,
}

/// One thing a merchant sells.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct VendorItem {
    pub item: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub price: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stack: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stock: Option<i64>,
    /// Only ever `true`: paid for with something other than copper.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub currency: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Trainer {
    #[serde(flatten)]
    pub npc: Npc,
    pub services: Vec<Service>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Vendor {
    #[serde(flatten)]
    pub npc: Npc,
    pub items: Vec<VendorItem>,
}

/// Everything one file has to send, npcs and their rows in ascending id order: `pairs()` walks a
/// Lua table in no order at all, and the batches must come out the same on every pass.
#[derive(Debug, Clone, PartialEq)]
pub struct Sources {
    pub passport: ClientPassport,
    pub trainers: Vec<Trainer>,
    pub vendors: Vec<Vendor>,
}

/// A whole number within `min..=int4`, or absent: the addon writes ids and copper as integers, so
/// a fraction or a number out of range is damage, and one bad value would take the whole request
/// down with it.
fn whole(t: &Table, key: &str, min: i64) -> Option<i64> {
    opt_exact_int(t, key).filter(|v| (min..=ITEM_MAX_ID).contains(v))
}

fn text(t: &Table, key: &str) -> Option<String> {
    opt_string(t, key).filter(|s| !s.is_empty() && s.chars().count() <= MAX_TEXT_CHARS)
}

fn service(spell: i64, t: &Table) -> Service {
    Service { spell, skill: whole(t, "r", 0), cost: whole(t, "c", 0), level: whole(t, "lv", 0), item: whole(t, "i", 1) }
}

fn vendor_item(item: i64, t: &Table) -> VendorItem {
    VendorItem {
        item,
        price: whole(t, "p", 0),
        stack: whole(t, "k", 1),
        stock: whole(t, "a", -1),
        currency: flag(t, "x").then_some(true),
    }
}

/// One of the store's two lists, or None when it is not a table (the store is then not one this
/// version understands). An npc without an id, a visit time or a single readable row is skipped
/// alone.
fn read_npcs<R>(store: &Table, list: &str, rows_key: &str, row: impl Fn(i64, &Table) -> R) -> Option<Vec<(Npc, Vec<R>)>> {
    let Ok(Value::Table(npcs)) = store.get::<Value>(list) else { return None };
    let mut out = BTreeMap::new();
    for pair in npcs.pairs::<Value, Value>() {
        let Ok((key, Value::Table(entry))) = pair else { continue };
        let (Some(id), Some(at)) = (item_id(&key), opt_exact_int(&entry, "at").filter(|at| *at > 0)) else { continue };
        let Ok(Value::Table(listed)) = entry.get::<Value>(rows_key) else { continue };
        let mut rows = BTreeMap::new();
        for pair in listed.pairs::<Value, Value>() {
            let Ok((key, Value::Table(fields))) = pair else { continue };
            if let Some(row_id) = item_id(&key) {
                rows.insert(row_id, row(row_id, &fields));
            }
        }
        if rows.is_empty() {
            continue;
        }
        let npc = Npc {
            npc: id,
            name: text(&entry, "n"),
            map: whole(&entry, "m", 1),
            zone: text(&entry, "z"),
            subzone: text(&entry, "s"),
            faction: opt_string(&entry, "f").filter(|f| FACTIONS.contains(&f.as_str())),
            at,
        };
        out.insert(id, (npc, rows.into_values().take(MAX_ROWS).collect()));
    }
    Some(out.into_values().collect())
}

/// The trainers and merchants one SavedVariables file holds, or None: not a WoW: Forever file,
/// no `foreverSources`, a store of another version or shape, or nothing in it worth a request.
pub fn read_sources(lua_source: &str) -> Result<Option<Sources>, String> {
    let lua = Lua::new_with(StdLib::NONE, LuaOptions::default()).map_err(|e| e.to_string())?;
    lua.load(lua_source).exec().map_err(|e| e.to_string())?;
    let Ok(Value::Table(db)) = lua.globals().get::<Value>("GoldCapDB") else { return Ok(None) };
    // Only a file that says it is WoW: Forever's. An unstamped one is unknown, not Forever.
    let Some(passport) = crate::savedvars::read_client(&db).filter(|p| crate::forever::is_forever_interface(p.interface)) else {
        return Ok(None);
    };
    let Ok(Value::Table(store)) = db.get::<Value>("foreverSources") else { return Ok(None) };
    if opt_exact_int(&store, "v") != Some(1) {
        return Ok(None);
    }
    let (Some(trainers), Some(vendors)) = (read_npcs(&store, "trainers", "sv", service), read_npcs(&store, "vendors", "it", vendor_item))
    else {
        return Ok(None);
    };
    if trainers.is_empty() && vendors.is_empty() {
        return Ok(None);
    }
    Ok(Some(Sources {
        passport,
        trainers: trainers.into_iter().map(|(npc, services)| Trainer { npc, services }).collect(),
        vendors: vendors.into_iter().map(|(npc, items)| Vendor { npc, items }).collect(),
    }))
}

/// The client language of the install a SavedVariables file belongs to, from its
/// `WTF/Config.wtf`: the names and zones in the file are in that language. The file is
/// `<install>/WTF/Account/<account>/SavedVariables/GoldCap.lua`, so `WTF` is four levels up.
/// No Config.wtf, or no locale in it, is simply no locale.
pub fn locale_for(file: &Path) -> Option<String> {
    let config = std::fs::read_to_string(file.ancestors().nth(4)?.join("Config.wtf")).ok()?;
    crate::wtf::parse_text_locale(&config)
}

/// One npc of either kind, waiting to go up.
#[derive(Clone, Copy)]
pub enum Pending<'a> {
    Trainer(&'a Trainer),
    Vendor(&'a Vendor),
}

impl Pending<'_> {
    fn npc(&self) -> &Npc {
        match self {
            Pending::Trainer(t) => &t.npc,
            Pending::Vendor(v) => &v.npc,
        }
    }

    /// The key it is remembered under: a trainer and a merchant can be the same npc.
    fn key(&self) -> String {
        match self {
            Pending::Trainer(t) => format!("t:{}", t.npc.npc),
            Pending::Vendor(v) => format!("v:{}", v.npc.npc),
        }
    }
}

/// Every npc of the file, trainers first, each kind by id.
fn all(sources: &Sources) -> Vec<Pending<'_>> {
    sources.trainers.iter().map(Pending::Trainer).chain(sources.vendors.iter().map(Pending::Vendor)).collect()
}

#[derive(Serialize)]
struct PassportBody<'a> {
    interface: i64,
    build: &'a str,
}

#[derive(Serialize)]
struct UploadBody<'a> {
    client: PassportBody<'a>,
    #[serde(skip_serializing_if = "Option::is_none")]
    locale: Option<&'a str>,
    trainers: Vec<&'a Trainer>,
    vendors: Vec<&'a Vendor>,
}

fn body<'a>(passport: &'a ClientPassport, locale: Option<&'a str>, batch: &[Pending<'a>]) -> UploadBody<'a> {
    let mut body = UploadBody {
        client: PassportBody { interface: passport.interface, build: &passport.build },
        locale,
        trainers: Vec::new(),
        vendors: Vec::new(),
    };
    for npc in batch {
        match npc {
            Pending::Trainer(t) => body.trainers.push(t),
            Pending::Vendor(v) => body.vendors.push(v),
        }
    }
    body
}

pub enum SourcesOutcome {
    /// Taken, or refused for good as it is (400/413/422).
    Done,
    /// 409: not a game or a region the site takes. 404: a site that does not have the route yet.
    /// Nothing is marked sent; the file goes up again only when it changes.
    Refused,
    Unauthorized,
    Retry(String),
}

/// One batch to the Forever route under the pairing token and the file's own Forever passport.
pub async fn upload_sources(
    client: &reqwest::Client,
    base: &str,
    token: &str,
    passport: &ClientPassport,
    locale: Option<&str>,
    batch: &[Pending<'_>],
) -> SourcesOutcome {
    let sent = client
        .post(format!("{base}/v1/forever/sources"))
        .bearer_auth(token)
        .header(crate::upload::CLIENT_HEADER, passport.header_value())
        .json(&body(passport, locale, batch))
        .send()
        .await;
    match sent {
        Err(e) => SourcesOutcome::Retry(crate::sync::describe_err(&e, crate::sync::CLIENT_TIMEOUT)),
        Ok(res) => match res.status().as_u16() {
            // 400/413/422: this batch will never be taken as it is, so resending it every tick
            // helps nobody. Marked sent like a 200.
            200 | 400 | 413 | 422 => SourcesOutcome::Done,
            404 | 409 => SourcesOutcome::Refused,
            401 => SourcesOutcome::Unauthorized,
            code => SourcesOutcome::Retry(format!("status {code}")),
        },
    }
}

/// One file's pass: read it, forget npcs it no longer holds, send the new and the revisited in
/// batches. A file with nothing to send (an older addon, another game) makes no request at all.
pub async fn send_sources(
    client: &reqwest::Client,
    base: &str,
    token: &str,
    file: &Path,
    sent: &mut BTreeMap<String, i64>,
    logger: &crate::logging::Logger,
) -> PurchasePass {
    let Ok(source) = std::fs::read_to_string(file) else { return PurchasePass::Incomplete };
    let sources = match read_sources(&source) {
        Ok(Some(s)) => s,
        Ok(None) => {
            sent.clear();
            return PurchasePass::Complete;
        }
        Err(e) => {
            logger.error(&format!("{}: not readable yet ({e})", file.display()));
            return PurchasePass::Incomplete;
        }
    };
    let npcs = all(&sources);
    let present: BTreeMap<String, i64> = npcs.iter().map(|n| (n.key(), n.npc().at)).collect();
    sent.retain(|k, _| present.contains_key(k));
    let pending: Vec<Pending<'_>> = npcs.into_iter().filter(|n| sent.get(&n.key()) != Some(&n.npc().at)).collect();
    if pending.is_empty() {
        return PurchasePass::Complete;
    }
    let locale = locale_for(file);
    let mut taken = 0usize;
    for batch in pending.chunks(MAX_BATCH_NPCS) {
        match upload_sources(client, base, token, &sources.passport, locale.as_deref(), batch).await {
            SourcesOutcome::Done => {
                sent.extend(batch.iter().map(|n| (n.key(), n.npc().at)));
                taken += batch.len();
            }
            SourcesOutcome::Refused => break,
            SourcesOutcome::Unauthorized => return PurchasePass::Unauthorized,
            SourcesOutcome::Retry(why) => {
                logger.error(&format!("forever sources from {}: will retry ({why})", file.display()));
                return PurchasePass::Incomplete;
            }
        }
    }
    if taken > 0 {
        logger.info(&format!("forever sources from {}: sent ({taken} trainers and merchants)", file.display()));
    }
    PurchasePass::Complete
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forever::tests::{answer, serve_forever, serve_once};

    const CLIENT: &str = r#"client = { interface = 16001, build = "1.60.1.70009", regionId = 90 }"#;

    /// One trainer (a recipe and a rank that makes no item) and two merchants, with the things a
    /// real file can hold that must not go up or must not break the rest.
    fn fixture() -> String {
        format!(
            r#"GoldCapDB = {{ {CLIENT}, foreverSources = {{ v = 1,
              trainers = {{
                [11044] = {{ n = "Doctor Martin Felben", m = 1458, z = "Undercity", s = "The Apothecarium", f = "Horde", at = 1794681535,
                  sv = {{ [3101] = {{ l = "Alchemy", r = 50, c = 500, lv = 10 }},
                         [2334] = {{ l = "Alchemy", r = 50, c = 225, lv = 0, i = 2458 }},
                         junk = {{ r = 1 }}, [7] = "not a row" }} }},
                [11045] = {{ n = "Never Visited", sv = {{ [1] = {{ r = 1 }} }} }},
                [11046] = {{ n = "Teaches Nothing", at = 1794681600, sv = {{ }} }},
                [2147483648] = {{ n = "No Such Npc", at = 1794681600, sv = {{ [1] = {{ r = 1 }} }} }},
              }},
              vendors = {{
                [9999] = {{ n = "{long}", m = 1.5, z = "Stranglethorn Vale", f = "Neutral", at = 1794700300,
                  it = {{ [159] = {{ p = 25, k = 5, a = -1, c = 0, sc = 5 }} }} }},
                [3534] = {{ n = "Wallace the Blind", m = 1421, z = "Silverpine Forest", s = "The Greymane Wall", f = "", at = 1794700233,
                  it = {{ [21099] = {{ p = 0, k = 1, a = 2, x = true }},
                         [4565] = {{ k = 1, a = -1, x = false }},
                         [1197] = {{ p = 2666, k = 1, a = -1, c = 2, sc = 7 }},
                         [2488] = {{ p = 12.5, k = 0, a = -2 }} }} }},
                [3535] = {{ n = "Half Open", at = 1794700250.5, it = {{ [1] = {{ p = 1 }} }} }},
              }} }} }}"#,
            long = "N".repeat(65)
        )
    }

    /// `<root>/_forever_/WTF/Account/A/SavedVariables/GoldCap.lua`, with a Config.wtf when asked.
    fn install(label: &str, lua: &str, config_wtf: Option<&str>) -> (std::path::PathBuf, std::path::PathBuf) {
        let root = std::env::temp_dir().join(format!("goldcap-forever-sources-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let wtf = root.join("_forever_").join("WTF");
        let sv = wtf.join("Account").join("A").join("SavedVariables");
        std::fs::create_dir_all(&sv).unwrap();
        if let Some(config) = config_wtf {
            std::fs::write(wtf.join("Config.wtf"), config).unwrap();
        }
        let file = sv.join("GoldCap.lua");
        std::fs::write(&file, lua).unwrap();
        (root, file)
    }

    fn body_of(request: &str) -> serde_json::Value {
        serde_json::from_str(request.split("\r\n\r\n").nth(1).unwrap()).unwrap()
    }

    fn ids(body: &serde_json::Value, list: &str) -> Vec<i64> {
        body[list].as_array().unwrap().iter().map(|n| n["npc"].as_i64().unwrap()).collect()
    }

    /// `trainers` trainers (ids from 1) and `vendors` merchants (ids from 101), one row each.
    fn many(trainers: i64, vendors: i64) -> String {
        let t: String = (1..=trainers).map(|i| format!("[{i}] = {{ at = 1794681535, sv = {{ [2334] = {{ c = 225 }} }} }},")).collect();
        let v: String = (101..101 + vendors).map(|i| format!("[{i}] = {{ at = 1794700233, it = {{ [1197] = {{ p = 2666 }} }} }},")).collect();
        format!("GoldCapDB = {{ {CLIENT}, foreverSources = {{ v = 1, trainers = {{ {t} }}, vendors = {{ {v} }} }} }}")
    }

    #[test]
    fn the_store_becomes_exactly_the_contract_body() {
        let sources = read_sources(&fixture()).unwrap().expect("sources");
        let json = serde_json::to_value(body(&sources.passport, Some("enUS"), &all(&sources))).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "client": { "interface": 16001, "build": "1.60.1.70009" },
                "locale": "enUS",
                "trainers": [
                    { "npc": 11044, "name": "Doctor Martin Felben", "map": 1458, "zone": "Undercity", "subzone": "The Apothecarium", "faction": "Horde", "at": 1794681535,
                      "services": [
                          { "spell": 2334, "skill": 50, "cost": 225, "level": 0, "item": 2458 },
                          { "spell": 3101, "skill": 50, "cost": 500, "level": 10 }
                      ] }
                ],
                "vendors": [
                    { "npc": 3534, "name": "Wallace the Blind", "map": 1421, "zone": "Silverpine Forest", "subzone": "The Greymane Wall", "at": 1794700233,
                      "items": [
                          { "item": 1197, "price": 2666, "stack": 1, "stock": -1 },
                          { "item": 2488 },
                          { "item": 4565, "stack": 1, "stock": -1 },
                          { "item": 21099, "price": 0, "stack": 1, "stock": 2, "currency": true }
                      ] },
                    { "npc": 9999, "zone": "Stranglethorn Vale", "at": 1794700300,
                      "items": [ { "item": 159, "price": 25, "stack": 5, "stock": -1 } ] }
                ]
            })
        );
        // Without a locale the key is not there at all.
        let json = serde_json::to_value(body(&sources.passport, None, &all(&sources))).unwrap();
        assert!(json.get("locale").is_none());
    }

    #[test]
    fn a_text_field_is_measured_in_characters() {
        let name = "ы".repeat(64);
        let lua = format!(r#"GoldCapDB = {{ {CLIENT}, foreverSources = {{ v = 1, trainers = {{ }}, vendors = {{ [1] = {{ n = "{name}", at = 5, it = {{ [2] = {{ }} }} }} }} }} }}"#);
        let sources = read_sources(&lua).unwrap().unwrap();
        assert_eq!(sources.vendors[0].npc.name.as_deref(), Some(name.as_str()));
        assert!(sources.trainers.is_empty());
    }

    #[test]
    fn an_npc_with_too_many_rows_sends_the_lowest_ids() {
        let rows: String = (1..=MAX_ROWS as i64 + 50).rev().map(|i| format!("[{i}] = {{ p = 1 }},")).collect();
        let lua = format!("GoldCapDB = {{ {CLIENT}, foreverSources = {{ v = 1, trainers = {{ }}, vendors = {{ [1] = {{ at = 5, it = {{ {rows} }} }} }} }} }}");
        let items = &read_sources(&lua).unwrap().unwrap().vendors[0].items;
        assert_eq!(items.len(), MAX_ROWS);
        assert_eq!((items[0].item, items[MAX_ROWS - 1].item), (1, MAX_ROWS as i64));
    }

    #[tokio::test]
    async fn a_file_with_nothing_to_send_makes_no_request() {
        let full = fixture();
        let cases = [
            ("another version", full.replace("v = 1,", "v = 2,")),
            ("no version", full.replace("v = 1,", "")),
            ("no store", format!("GoldCapDB = {{ {CLIENT}, ledger = {{ }} }}")),
            ("a store that is not a table", format!("GoldCapDB = {{ {CLIENT}, foreverSources = true }}")),
            ("empty lists", format!("GoldCapDB = {{ {CLIENT}, foreverSources = {{ v = 1, trainers = {{ }}, vendors = {{ }} }} }}")),
            ("one list missing", format!("GoldCapDB = {{ {CLIENT}, foreverSources = {{ v = 1, trainers = {{ [1] = {{ at = 5, sv = {{ [2] = {{ }} }} }} }} }} }}")),
            ("a list that is not a table", format!("GoldCapDB = {{ {CLIENT}, foreverSources = {{ v = 1, trainers = 3, vendors = {{ [1] = {{ at = 5, it = {{ [2] = {{ }} }} }} }} }} }}")),
            ("only npcs that are skipped", format!("GoldCapDB = {{ {CLIENT}, foreverSources = {{ v = 1, trainers = {{ [1] = {{ sv = {{ [2] = {{ }} }} }} }}, vendors = {{ [1] = {{ at = 5, it = {{ }} }} }} }} }}")),
            ("retail", full.replace("interface = 16001, build = \"1.60.1.70009\"", "interface = 120100, build = \"12.1.0.69933\"")),
            ("no passport", full.replace(&format!("{CLIENT},"), "")),
            ("no GoldCapDB", "GoldCapOther = { }".to_string()),
        ];
        let (root, file) = install("silent", "", Some("SET textLocale \"enUS\"\n"));
        let logger = crate::logging::Logger::new(&root.join("logs")).unwrap();
        for (what, lua) in cases {
            assert_eq!(read_sources(&lua).unwrap(), None, "{what}");
            std::fs::write(&file, &lua).unwrap();
            let mut sent = BTreeMap::new();
            // Nothing listens on port 9: a request would come back as Incomplete.
            let pass = send_sources(&crate::sync::build_client(), "http://127.0.0.1:9", "tok", &file, &mut sent, &logger).await;
            assert!(matches!(pass, PurchasePass::Complete), "{what}");
            assert!(sent.is_empty(), "{what}");
        }
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn the_locale_is_read_from_the_installs_config_wtf() {
        let (root, file) = install("locale", "", Some("SET portal \"EU\"\nSET textLocale \"zhCN\"\n"));
        assert_eq!(locale_for(&file), Some("zhCN".to_string()));
        std::fs::write(root.join("_forever_/WTF/Config.wtf"), "SET portal \"EU\"\n").unwrap();
        assert_eq!(locale_for(&file), None);
        std::fs::remove_file(root.join("_forever_/WTF/Config.wtf")).unwrap();
        assert_eq!(locale_for(&file), None);
        assert_eq!(locale_for(Path::new("GoldCap.lua")), None, "a path too short to have an install");
        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn the_upload_names_the_forever_passport_and_goes_to_the_sources_route() {
        let (root, file) = install("route", &fixture(), Some("SET textLocale \"enUS\"\n"));
        let logger = crate::logging::Logger::new(&root.join("logs")).unwrap();
        let (base, seen) = serve_once(answer("200 OK", r#"{"accepted":3,"skipped":0}"#));
        let pass = send_sources(&crate::sync::build_client(), &base, "tok", &file, &mut BTreeMap::new(), &logger).await;
        assert!(matches!(pass, PurchasePass::Complete));
        let request = seen.recv().unwrap();
        let lower = request.to_ascii_lowercase();
        assert!(lower.starts_with("post /v1/forever/sources "), "{request}");
        assert!(lower.contains("x-goldcap-client: 16001/1.60.1.70009"));
        assert!(lower.contains("authorization: bearer tok"));
        assert!(request.contains(r#""client":{"interface":16001,"build":"1.60.1.70009"}"#));
        assert!(request.contains(r#""locale":"enUS""#), "{request}");
        assert_eq!((ids(&body_of(&request), "trainers"), ids(&body_of(&request), "vendors")), (vec![11044], vec![3534, 9999]));
        // What the addon keeps for itself stays in the file.
        assert!(!request.contains("Alchemy") && !request.contains("\"l\"") && !request.contains("\"sc\""), "{request}");

        // No Config.wtf: the same body without the key.
        std::fs::remove_file(root.join("_forever_/WTF/Config.wtf")).unwrap();
        let (base, seen) = serve_once(answer("200 OK", r#"{"accepted":3,"skipped":0}"#));
        send_sources(&crate::sync::build_client(), &base, "tok", &file, &mut BTreeMap::new(), &logger).await;
        let request = seen.recv().unwrap();
        assert!(!request.contains("locale"), "{request}");
        assert!(body_of(&request).get("trainers").is_some());
        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn an_npc_is_marked_with_its_visit_only_when_the_site_took_it_and_forgotten_when_gone() {
        let (root, file) = install("sent", &fixture(), None);
        let logger = crate::logging::Logger::new(&root.join("logs")).unwrap();
        let mut sent: BTreeMap<String, i64> = [("t:777".to_string(), 5)].into();

        let (base, _seen) = serve_once(answer("500 Internal Server Error", "{}"));
        let pass = send_sources(&crate::sync::build_client(), &base, "tok", &file, &mut sent, &logger).await;
        assert!(matches!(pass, PurchasePass::Incomplete));
        assert!(sent.is_empty(), "an npc no longer in the file is forgotten; nothing was taken");

        let (base, _seen) = serve_once(answer("200 OK", r#"{"accepted":3,"skipped":0}"#));
        let pass = send_sources(&crate::sync::build_client(), &base, "tok", &file, &mut sent, &logger).await;
        assert!(matches!(pass, PurchasePass::Complete));
        let all_three: BTreeMap<String, i64> =
            [("t:11044".to_string(), 1794681535), ("v:3534".to_string(), 1794700233), ("v:9999".to_string(), 1794700300)].into();
        assert_eq!(sent, all_three);

        // Nothing pending: no request at all (nothing listens on port 9).
        let pass = send_sources(&crate::sync::build_client(), "http://127.0.0.1:9", "tok", &file, &mut sent, &logger).await;
        assert!(matches!(pass, PurchasePass::Complete));
        assert_eq!(sent, all_three);

        // The player opened one merchant again: that one goes up, alone.
        let revisited = fixture().replace("at = 1794700233", "at = 1794790000");
        std::fs::write(&file, &revisited).unwrap();
        let (base, seen) = serve_once(answer("200 OK", r#"{"accepted":1,"skipped":0}"#));
        let pass = send_sources(&crate::sync::build_client(), &base, "tok", &file, &mut sent, &logger).await;
        assert!(matches!(pass, PurchasePass::Complete));
        let again = body_of(&seen.recv().unwrap());
        assert_eq!((ids(&again, "trainers"), ids(&again, "vendors")), (vec![], vec![3534]));
        assert_eq!(sent["v:3534"], 1794790000);
        assert_eq!(sent.len(), 3);

        // An npc gone from the file leaves the map, with no request.
        std::fs::write(&file, revisited.replace("[11044] =", "gone =")).unwrap();
        let pass = send_sources(&crate::sync::build_client(), "http://127.0.0.1:9", "tok", &file, &mut sent, &logger).await;
        assert!(matches!(pass, PurchasePass::Complete));
        assert_eq!(sent.keys().map(String::as_str).collect::<Vec<_>>(), vec!["v:3534", "v:9999"]);
        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn npcs_go_up_twenty_five_at_a_time_and_a_taken_batch_stays_taken() {
        let (root, file) = install("batches", &many(10, 20), None);
        let logger = crate::logging::Logger::new(&root.join("logs")).unwrap();
        let mut sent = BTreeMap::new();
        let (base, seen) = serve_forever(vec![answer("200 OK", r#"{"accepted":25,"skipped":0}"#), answer("500 Internal Server Error", "{}")]);
        let pass = send_sources(&crate::sync::build_client(), &base, "tok", &file, &mut sent, &logger).await;
        assert!(matches!(pass, PurchasePass::Incomplete));
        let requests: Vec<serde_json::Value> = seen.try_iter().map(|r| body_of(&r)).collect();
        assert_eq!(requests.len(), 2);
        assert_eq!(ids(&requests[0], "trainers"), (1..=10).collect::<Vec<i64>>());
        assert_eq!(ids(&requests[0], "vendors"), (101..=115).collect::<Vec<i64>>());
        assert_eq!(requests[1]["trainers"], serde_json::json!([]));
        assert_eq!(ids(&requests[1], "vendors"), (116..=120).collect::<Vec<i64>>());
        assert_eq!(sent.len(), MAX_BATCH_NPCS, "the first batch is kept");
        assert!(sent.contains_key("t:10") && sent.contains_key("v:115") && !sent.contains_key("v:116"));

        // Next pass: only the five the site did not take.
        let (base, seen) = serve_once(answer("200 OK", r#"{"accepted":5,"skipped":0}"#));
        let pass = send_sources(&crate::sync::build_client(), &base, "tok", &file, &mut sent, &logger).await;
        assert!(matches!(pass, PurchasePass::Complete));
        assert_eq!(ids(&body_of(&seen.recv().unwrap()), "vendors"), (116..=120).collect::<Vec<i64>>());
        assert_eq!(sent.len(), 30);
        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn a_batch_refused_for_good_is_marked_like_a_taken_one() {
        let (root, file) = install("refused-for-good", &fixture(), None);
        let logger = crate::logging::Logger::new(&root.join("logs")).unwrap();
        for status in ["400 Bad Request", "413 Payload Too Large", "422 Unprocessable Entity"] {
            let mut sent = BTreeMap::new();
            let (base, _seen) = serve_once(answer(status, r#"{"error":"bad_request"}"#));
            let pass = send_sources(&crate::sync::build_client(), &base, "tok", &file, &mut sent, &logger).await;
            assert!(matches!(pass, PurchasePass::Complete), "{status}");
            assert_eq!(sent.len(), 3, "{status}");
        }
        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn a_refused_token_stops_the_pass_and_a_site_that_will_not_take_it_marks_nothing() {
        let (root, file) = install("refusals", &many(10, 20), None);
        let logger = crate::logging::Logger::new(&root.join("logs")).unwrap();
        let mut sent = BTreeMap::new();

        let (base, _seen) = serve_once(answer("401 Unauthorized", r#"{"error":"unauthorized"}"#));
        let pass = send_sources(&crate::sync::build_client(), &base, "tok", &file, &mut sent, &logger).await;
        assert!(matches!(pass, PurchasePass::Unauthorized));
        assert!(sent.is_empty());

        // 404 is a site without the route yet, 409 one that does not take this game or region.
        // Complete, so the file's stamp is kept and it goes again only when it changes; nothing is
        // marked sent, so a site that learns the route later still gets every npc.
        for status in ["404 Not Found", "409 Conflict"] {
            let (base, seen) = serve_forever(vec![answer(status, r#"{"error":"nope"}"#), answer("200 OK", "{}")]);
            let pass = send_sources(&crate::sync::build_client(), &base, "tok", &file, &mut sent, &logger).await;
            assert!(matches!(pass, PurchasePass::Complete), "{status}");
            assert!(sent.is_empty(), "{status}");
            assert_eq!(seen.try_iter().count(), 1, "{status}: no second batch after the first was refused");
        }

        // Too many requests is not a refusal: it is tried again.
        let (base, _seen) = serve_once(answer("429 Too Many Requests", r#"{"error":"rate_limited"}"#));
        let pass = send_sources(&crate::sync::build_client(), &base, "tok", &file, &mut sent, &logger).await;
        assert!(matches!(pass, PurchasePass::Incomplete));
        let pass = send_sources(&crate::sync::build_client(), "http://127.0.0.1:9", "tok", &file, &mut sent, &logger).await;
        assert!(matches!(pass, PurchasePass::Incomplete), "nobody listening at all");
        assert!(sent.is_empty());
        std::fs::remove_dir_all(&root).ok();
    }
}
