//! WoW: Forever. The player's own auction house scan (`GoldCapDB.foreverScan.fold`) goes up
//! to goldcap.gg, and every player's prices come back into the Forever install's
//! `GoldCap_AppData`. The wire is the site's (plan 2a Tasks 3 and 9; docs/companion/AGENTS.md
//! "WoW: Forever upload contract").

use mlua::{Lua, LuaOptions, StdLib, Table, Value};
use serde::Serialize;
use std::collections::BTreeMap;

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
}
