//! Wire shape, `Runs.lua` renderer, fetch, and write seam for the "Buy runs"
//! feature — same shape as `ledger_summary.rs`, rides the sync tick as a
//! passenger after it (see `sync.rs::sync_once`). Contract v1 carried a line's
//! id, quantity, vendor flag and English name; v2 adds the site's reference
//! price, the vendor's unit price and the region's cheap hour, all optional,
//! so a v1 server and a v2 server both render through the same code.
//! v3 adds, on a run, its kind (`k`), who shared it (`by`) and the plan it
//! came from (`src`), and on a line an absolute price ceiling (`cc`), the
//! realm an alert hit was seen on (`rl`) and the recipe that crafts it
//! (`cr`) — every one of them optional, and every one of them read through
//! `lenient_opt`, so a field this build cannot make sense of costs its own
//! fact and never the whole sync. Since 2026-09 a gear line may also carry
//! its member's lowest item level (`minIlvl`), read and written the same way.

use crate::ledger_summary::parse_iso_utc;
use crate::luafile::escape_lua_string;
use serde::Deserialize;
use std::path::Path;

/// A JSON number the site sends for a copper amount or a percentage. Postgres
/// hands back a median as a fraction often enough that a strict `i64` would
/// turn one `.5` into a permanently stale `Runs.lua` — the fetch would fail
/// on every tick and the write seam would (correctly) keep the old file. So a
/// float is rounded to the nearest whole unit; only a value no `i64` can hold
/// is refused.
#[derive(Deserialize)]
#[serde(untagged)]
enum LenientNumber {
    Int(i64),
    Float(f64),
}

impl LenientNumber {
    fn to_i64(&self) -> Option<i64> {
        match self {
            LenientNumber::Int(i) => Some(*i),
            LenientNumber::Float(f)
                if f.is_finite() && *f >= i64::MIN as f64 && *f <= i64::MAX as f64 =>
            {
                Some(f.round() as i64)
            }
            LenientNumber::Float(_) => None,
        }
    }
}

fn lenient_i64<'de, D>(d: D) -> Result<i64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error as _;
    LenientNumber::deserialize(d)?
        .to_i64()
        .ok_or_else(|| D::Error::custom("runs: number out of range"))
}

/// Reads an optional field without ever failing the payload around it: the
/// value is taken as plain JSON first and only then shaped into `T`, and
/// anything that does not fit — a number where a string belongs, an object
/// missing a key this build needs, a shape a later contract invented — reads
/// as absent. A fetch that fails writes nothing, so a strict reader here
/// would strand `Runs.lua` at its previous contents on every tick from then
/// on; one fact the companion cannot read is cheaper than that by a mile.
fn lenient_opt<'de, D, T>(d: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned,
{
    let raw = Option::<serde_json::Value>::deserialize(d)?;
    Ok(raw.and_then(|v| serde_json::from_value::<T>(v).ok()))
}

/// `lenient_opt` for a copper amount: a fraction rounds (see `LenientNumber`)
/// and anything that is not a number at all reads as absent.
fn lenient_opt_number<'de, D>(d: D) -> Result<Option<i64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw: Option<LenientNumber> = lenient_opt(d)?;
    Ok(raw.and_then(|n| n.to_i64()))
}

/// `lenient_opt` for a list: each entry is shaped on its own, and one that
/// does not fit is dropped by itself — a thousand caps must not vanish
/// because the site once sent a `"i":"nope"`. A missing or non-list value
/// reads as empty.
fn lenient_vec<'de, D, T>(d: D) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned,
{
    let raw = Option::<serde_json::Value>::deserialize(d)?;
    Ok(match raw {
        Some(serde_json::Value::Array(items)) => items
            .into_iter()
            .filter_map(|v| serde_json::from_value::<T>(v).ok())
            .collect(),
        _ => Vec::new(),
    })
}

/// `lenient_vec` for names a cap addresses by position (`caps[].g`). A name
/// that is not a string keeps its slot as an empty one instead of being
/// dropped: dropping it would shift every later name down one and file each
/// cap after it under its neighbour's group. The render then gives no cap a
/// label that points at the empty slot. A missing or non-list value reads as
/// empty.
fn lenient_names<'de, D>(d: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = Option::<serde_json::Value>::deserialize(d)?;
    Ok(match raw {
        Some(serde_json::Value::Array(items)) => items
            .into_iter()
            .map(|v| match v {
                serde_json::Value::String(s) => s,
                _ => String::new(),
            })
            .collect(),
        _ => Vec::new(),
    })
}

/// When the region's last 14 days say this item is usually cheapest. `hour`
/// is 0–23 in UTC (the addon converts to server time), `pct` is how far under
/// the overall mean that hour sits, as a negative whole percent.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WireCheapHour {
    #[serde(deserialize_with = "lenient_i64")]
    pub hour: i64,
    #[serde(deserialize_with = "lenient_i64")]
    pub pct: i64,
}

/// Where a run came from, as the site would say it out loud: "Cooking 1–100".
/// The wire also carries a `kind` ("profession" today); the companion does not
/// read it, because the label is what the addon shows either way and serde
/// drops what nothing asks for.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WireRunSource {
    pub label: String,
}

/// The realm an alert hit was seen on. The addon shows it after the item
/// name; searching the auction house on another realm simply finds nothing.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WireRealm {
    pub id: u32,
    pub name: String,
}

/// One reagent of one craft, priced the same way a run line is: `usual` is
/// the site's reference price per unit and `vendor_unit` what a vendor
/// charges, both in copper, both absent when nobody knows.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WireCraftReagent {
    pub item_id: u32,
    pub qty: u32,
    pub name_en: String,
    pub vendor: bool,
    #[serde(default, deserialize_with = "lenient_opt_number")]
    pub usual: Option<i64>,
    #[serde(default, deserialize_with = "lenient_opt_number")]
    pub vendor_unit: Option<i64>,
}

/// The cheapest fully priced recipe that makes this line's item, as the site
/// costed it at fetch time: what one craft yields, what one craft's reagents
/// cost at the region's prices, and the reagents themselves. The addon does
/// the craft-or-buy comparison; the companion only carries it.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WireCraft {
    pub recipe_id: u32,
    pub crafted_qty: u32,
    #[serde(deserialize_with = "lenient_i64")]
    pub cost_copper: i64,
    pub reagents: Vec<WireCraftReagent>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WireRunLine {
    pub item_id: u32,
    pub qty: u32,
    pub vendor: bool,
    pub name_en: String,
    /// The site's reference price per unit at fetch time, in copper. Contract
    /// v2 and later; absent on a v1 response and on a line the site has no
    /// price for, and then the addon falls back to the import's `mv`.
    #[serde(default, deserialize_with = "lenient_opt_number")]
    pub usual: Option<i64>,
    /// What a vendor charges per unit, in copper. Absent when nobody sells it.
    #[serde(default, deserialize_with = "lenient_opt_number")]
    pub vendor_unit: Option<i64>,
    #[serde(default, deserialize_with = "lenient_opt")]
    pub cheap_hour: Option<WireCheapHour>,
    /// An absolute ceiling per unit, in copper — an alert run's target price.
    /// Contract v3 and later. Absent means the addon's own USUAL × cap%, as
    /// before; it is not the same as "no ceiling".
    #[serde(default, deserialize_with = "lenient_opt_number")]
    pub cap: Option<i64>,
    /// Set when the line is a realm-bound alert hit rather than a commodity.
    #[serde(default, deserialize_with = "lenient_opt")]
    pub realm: Option<WireRealm>,
    /// Set when a recipe makes this item and the site could price it whole.
    #[serde(default, deserialize_with = "lenient_opt")]
    pub craft: Option<WireCraft>,
    /// The lowest item level that counts, on a gear line whose alert-group
    /// member has one. Added 2026-09; absent means any level does. Anything
    /// that is not a whole number from 0 to `u32::MAX` reads as absent — the
    /// line loses its floor, never itself. 0 means no floor and is not
    /// written.
    #[serde(default, deserialize_with = "lenient_opt")]
    pub min_ilvl: Option<u32>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WireRun {
    pub code: String,
    #[serde(default, deserialize_with = "lenient_opt")]
    pub name: Option<String>,
    pub updated_at: String,
    /// `"list"` or `"alert"`. Contract v3 and later; absent is a list, which
    /// is what v1 and v2 always meant.
    #[serde(default, deserialize_with = "lenient_opt")]
    pub kind: Option<String>,
    /// The owner's public display name, when this is a run the player follows
    /// rather than one they saved.
    #[serde(default, deserialize_with = "lenient_opt")]
    pub shared_by: Option<String>,
    #[serde(default, deserialize_with = "lenient_opt")]
    pub source: Option<WireRunSource>,
    pub lines: Vec<WireRunLine>,
}

/// One alert-group member the addon polls live at the auction house: the
/// site ships the rules, not the hits (those are the `k = 'alert'` runs).
/// Keys are one letter on the wire because a Pro account can hold thousands.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct WireCap {
    /// Item id.
    #[serde(rename = "i")]
    pub item_id: u32,
    /// Ceiling per unit, copper — the member's own price or the group's computed target.
    #[serde(rename = "c", deserialize_with = "lenient_i64")]
    pub cap: i64,
    /// Lowest item level that counts; 0 = any variant.
    #[serde(rename = "l", default)]
    pub min_ilvl: u32,
    /// 0-based index into `groups`. Absent or unreadable means the cap
    /// belongs to no group — never "the first one".
    #[serde(rename = "g", default, deserialize_with = "lenient_opt")]
    pub group: Option<u32>,
    /// True when `cap` was typed by the player rather than derived from the market.
    #[serde(rename = "m", default)]
    pub manual: bool,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WireRuns {
    pub v: u32,
    pub generated_at: String,
    pub plan: String,
    pub free_lines: u32,
    pub runs: Vec<WireRun>,
    /// Alert-group names, indexed by `caps[].group`. Absent before 2026-09.
    #[serde(default, deserialize_with = "lenient_names")]
    pub groups: Vec<String>,
    #[serde(default, deserialize_with = "lenient_vec")]
    pub caps: Vec<WireCap>,
}

/// A string the addon can put in front of a player, or nothing at all: an
/// empty or whitespace-only value would render as `by = ''` and read in game
/// as "from" with a hole where a name belongs.
fn shown_text(s: &str) -> Option<&str> {
    let t = s.trim();
    (!t.is_empty()).then_some(t)
}

/// A craft as the addon reads it, or nothing at all. Every guard here names
/// something the addon would otherwise have to do the impossible with: a
/// recipe it cannot identify, a yield it would divide a need by, a cost of
/// nothing to compare a price against, a split with no reagents to split
/// into, or a reagent with no item behind it or none of it actually needed.
/// Half a craft is worse than no craft.
fn render_craft(craft: &WireCraft) -> Option<String> {
    if craft.recipe_id == 0
        || craft.crafted_qty == 0
        || craft.cost_copper <= 0
        || craft.reagents.is_empty()
        || craft.reagents.iter().any(|r| r.item_id == 0 || r.qty == 0)
    {
        return None;
    }
    let mut out = format!(
        ", cr = {{ r = {}, n = {}, c = {}, i = {{ ",
        craft.recipe_id, craft.crafted_qty, craft.cost_copper
    );
    for (i, reagent) in craft.reagents.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        out.push_str(&format!(
            "{{ i = {}, q = {}, n = '{}', v = {}",
            reagent.item_id,
            reagent.qty,
            escape_lua_string(&reagent.name_en),
            reagent.vendor
        ));
        // Same rule as a run line's prices: unknown is absent, never zero.
        if let Some(usual) = reagent.usual.filter(|v| *v > 0) {
            out.push_str(&format!(", u = {usual}"));
        }
        if let Some(vendor_unit) = reagent.vendor_unit.filter(|v| *v > 0) {
            out.push_str(&format!(", vu = {vendor_unit}"));
        }
        out.push_str(" }");
    }
    out.push_str(" } }");
    Some(out)
}

/// Renders `Runs.lua`'s contents: the addon-side buy list, keyed off the
/// same short field names as the rest of `GoldCap_AppData` (`i`/`q`/`v`/`n`
/// on a line, plus `u`/`vu`/`ch`/`cp` when the site priced the line) to keep
/// the in-game table small. `updatedAt` on each run is re-derived from its
/// ISO timestamp the same way `LedgerSummary.lua`'s `sale_rows` does; an
/// unparseable timestamp fails the whole render rather than silently
/// rendering as the Unix epoch — `apply_fetch_result` must not write a file
/// that lies about a run's freshness.
pub fn render_runs_lua(runs: &WireRuns, generated_at: i64) -> Result<String, String> {
    let mut out = String::new();
    // `v` is rendered straight from the wire payload rather than hard-coded:
    // the version gate itself lives in `apply_fetch_result`, which refuses
    // any `v` outside `SUPPORTED_VERSIONS` before this function is called.
    out.push_str(&format!(
        "GoldCap_AppRuns = {{ v = {}, generatedAt = {}, plan = '{}', freeLines = {}, runs = {{ ",
        runs.v,
        generated_at,
        escape_lua_string(&runs.plan),
        runs.free_lines
    ));
    for (ri, run) in runs.runs.iter().enumerate() {
        if ri > 0 {
            out.push_str(", ");
        }
        out.push_str(&format!("{{ code = '{}', ", escape_lua_string(&run.code)));
        if let Some(name) = &run.name {
            out.push_str(&format!("name = '{}', ", escape_lua_string(name)));
        }
        // Contract v3 run facts. `k` is written only for an alert run: a
        // missing `k` is the ordinary list run v1 and v2 always meant, so
        // writing the default would put a key that says nothing into every
        // table in the file.
        if run.kind.as_deref() == Some("alert") {
            out.push_str("k = 'alert', ");
        }
        if let Some(by) = run.shared_by.as_deref().and_then(shown_text) {
            out.push_str(&format!("by = '{}', ", escape_lua_string(by)));
        }
        if let Some(src) = run.source.as_ref().and_then(|s| shown_text(&s.label)) {
            out.push_str(&format!("src = '{}', ", escape_lua_string(src)));
        }
        let updated_at = parse_iso_utc(&run.updated_at).ok_or_else(|| {
            format!(
                "runs: bad updatedAt {:?} on run {}",
                run.updated_at, run.code
            )
        })?;
        out.push_str(&format!("updatedAt = {}, lines = {{ ", updated_at));
        for (li, line) in run.lines.iter().enumerate() {
            if li > 0 {
                out.push_str(", ");
            }
            out.push_str(&format!(
                "{{ i = {}, q = {}, v = {}, n = '{}'",
                line.item_id,
                line.qty,
                line.vendor,
                escape_lua_string(&line.name_en)
            ));
            // Contract v2 facts. Each is optional and each is omitted when
            // absent rather than written as 0 or nil — the addon reads a
            // missing key as "the site didn't know", which is not the same
            // as "the site says zero".
            // A zero is not a price either: the site stores an unknown vendor price as
            // NULL, so a 0 here would be a line priced at nothing in game.
            if let Some(usual) = line.usual.filter(|v| *v > 0) {
                out.push_str(&format!(", u = {usual}"));
            }
            if let Some(vendor_unit) = line.vendor_unit.filter(|v| *v > 0) {
                out.push_str(&format!(", vu = {vendor_unit}"));
            }
            if let Some(cheap) = &line.cheap_hour {
                if (0..=23).contains(&cheap.hour) {
                    out.push_str(&format!(", ch = {}, cp = {}", cheap.hour, cheap.pct));
                }
            }
            // Contract v3 line facts, under the same rule as the v2 ones: a
            // fact the site did not send is absent from the table, and a
            // fact it sent in a shape the addon cannot act on is dropped
            // whole rather than written half.
            if let Some(cap) = line.cap.filter(|v| *v > 0) {
                out.push_str(&format!(", cc = {cap}"));
            }
            // A gear member's item-level floor, under the wire's own name.
            // Zero is no floor, so it is not written: a line without one
            // renders the bytes it always did.
            if let Some(min_ilvl) = line.min_ilvl.filter(|v| *v > 0) {
                out.push_str(&format!(", minIlvl = {min_ilvl}"));
            }
            if let Some(realm) = line
                .realm
                .as_ref()
                .filter(|r| r.id > 0 && shown_text(&r.name).is_some())
            {
                out.push_str(&format!(
                    ", rl = {{ id = {}, n = '{}' }}",
                    realm.id,
                    escape_lua_string(realm.name.trim())
                ));
            }
            if let Some(craft) = line.craft.as_ref().and_then(render_craft) {
                out.push_str(&craft);
            }
            out.push_str(" }");
        }
        out.push_str(" } }");
    }
    out.push_str(" }");
    // Alert-group caps, 2026-09: written after `runs` under the same rule as
    // every v2/v3 fact — absent when there is nothing usable, never an empty
    // table, so a server without them yields the file this build always
    // wrote. `g` goes out 1-based for Lua and only when it indexes a name the
    // addon can show: a cap with no group, one off the end, or one on a name
    // that did not survive the wire goes out without a label rather than
    // under a blank one.
    let usable: Vec<&WireCap> = runs
        .caps
        .iter()
        .filter(|c| c.item_id > 0 && c.cap > 0)
        .collect();
    if !usable.is_empty() {
        out.push_str(", groups = { ");
        for (gi, name) in runs.groups.iter().enumerate() {
            if gi > 0 {
                out.push_str(", ");
            }
            out.push_str(&format!("'{}'", escape_lua_string(name)));
        }
        out.push_str(" }, caps = { ");
        let labelled = |g: &u32| {
            runs.groups
                .get(*g as usize)
                .is_some_and(|name| shown_text(name).is_some())
        };
        for (ci, cap) in usable.iter().enumerate() {
            if ci > 0 {
                out.push_str(", ");
            }
            out.push_str(&format!("{{ i = {}, c = {}", cap.item_id, cap.cap));
            if cap.min_ilvl > 0 {
                out.push_str(&format!(", l = {}", cap.min_ilvl));
            }
            if let Some(g) = cap.group.filter(labelled) {
                out.push_str(&format!(", g = {}", g + 1));
            }
            if cap.manual {
                out.push_str(", m = true");
            }
            out.push_str(" }");
        }
        out.push_str(" }");
    }
    out.push_str(" }\n");
    Ok(out)
}

pub const RUNS_URL: &str = "https://api.goldcap.gg/v1/lists/companion";

/// Fetches the paired user's buy runs. The token IS the identity, same as
/// `ledger_summary::fetch_summary` — no userId on the route.
pub async fn fetch_runs(client: &reqwest::Client, token: &str) -> Result<WireRuns, String> {
    let res = client
        .get(RUNS_URL)
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !res.status().is_success() {
        return Err(format!("http {}", res.status()));
    }
    res.json::<WireRuns>().await.map_err(|e| e.to_string())
}

/// Runs contracts this build understands. v1 is the phase-1 shape; v2 adds
/// the optional per-line price facts; v3 adds a run's kind, who shared it and
/// where it came from, plus a line's absolute cap, its realm and the recipe
/// that crafts it. An unknown version is refused rather than rendered
/// half-understood — the addon would then be reading a file whose meaning
/// this build cannot vouch for, and a stale correct file beats a fresh
/// misread one.
const SUPPORTED_VERSIONS: [u32; 3] = [1, 2, 3];

/// The write seam `sync_once` calls: `Ok(true)` means it wrote, and every
/// failure — a fetch error, a version this build does not support, or an
/// unparseable `updatedAt` inside `render_runs_lua` — comes back as `Err`
/// before any filesystem operation, leaving a previously written `Runs.lua`
/// untouched. The rendered `GoldCap_AppRuns.v` is the version the server
/// sent, not a constant: a v1 server keeps producing a v1 file.
/// `Ok(false)` is part of the shared shape with `ledger_summary`'s seam but
/// is never actually returned here.
pub fn apply_fetch_result(
    dir: &Path,
    fetched: Result<WireRuns, String>,
    generated_at: i64,
) -> Result<bool, String> {
    let runs = fetched?;
    if !SUPPORTED_VERSIONS.contains(&runs.v) {
        return Err(format!("runs: unsupported version {}", runs.v));
    }
    let contents = render_runs_lua(&runs, generated_at)?;
    crate::luafile::ensure_toc(dir).map_err(|e| e.to_string())?;
    crate::luafile::write_atomic(&dir.join(crate::luafile::RUNS_FILE_NAME), &contents)
        .map_err(|e| e.to_string())?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(item_id: u32, qty: u32, vendor: bool, name_en: &str) -> WireRunLine {
        WireRunLine {
            item_id,
            qty,
            vendor,
            name_en: name_en.into(),
            usual: None,
            vendor_unit: None,
            cheap_hour: None,
            cap: None,
            realm: None,
            craft: None,
            min_ilvl: None,
        }
    }

    /// A run with none of the phase-3 facts, for `..plain_run()` in the
    /// tests below: the four fields every test already spells out are
    /// overridden by the literal, the three new ones come from here.
    fn plain_run() -> WireRun {
        WireRun {
            code: "abcd2345".into(),
            name: None,
            updated_at: "2026-09-15T09:00:00.000Z".into(),
            kind: None,
            shared_by: None,
            source: None,
            lines: vec![],
        }
    }

    /// A v3 payload with no runs and no caps, for the caps tests below: the
    /// fields every test already spells out are overridden by the literal.
    fn v3_empty() -> WireRuns {
        WireRuns {
            v: 3,
            generated_at: "2026-09-22T10:00:00.000Z".into(),
            plan: "pro".into(),
            free_lines: 5,
            runs: vec![],
            groups: vec![],
            caps: vec![],
        }
    }

    #[test]
    fn renders_the_addon_table_shape() {
        let runs = WireRuns {
            v: 1,
            generated_at: "2026-09-15T10:00:00.000Z".into(),
            plan: "pro".into(),
            free_lines: 5,
            groups: vec![],
            caps: vec![],
            runs: vec![WireRun {
                code: "abcd2345".into(),
                name: Some("Cooking 1-100".into()),
                updated_at: "2026-09-15T09:00:00.000Z".into(),
                lines: vec![line(5, 210, false, "Plant Protein")],
                ..plain_run()
            }],
        };
        let lua = render_runs_lua(&runs, 1_789_000_000).unwrap();
        let updated_at = crate::ledger_summary::parse_iso_utc("2026-09-15T09:00:00.000Z").unwrap();
        let expected = format!(
            "GoldCap_AppRuns = {{ v = 1, generatedAt = 1789000000, plan = 'pro', freeLines = 5, runs = {{ {{ code = 'abcd2345', name = 'Cooking 1-100', updatedAt = {}, lines = {{ {{ i = 5, q = 210, v = false, n = 'Plant Protein' }} }} }} }} }}\n",
            updated_at
        );
        assert_eq!(lua, expected);
    }

    #[test]
    fn a_nameless_run_omits_name_and_a_quote_in_a_name_is_escaped() {
        let runs = WireRuns {
            v: 1,
            generated_at: "2026-09-15T10:00:00.000Z".into(),
            plan: "free".into(),
            free_lines: 5,
            groups: vec![],
            caps: vec![],
            runs: vec![WireRun {
                code: "abcd2345".into(),
                name: None,
                updated_at: "2026-09-15T09:00:00.000Z".into(),
                lines: vec![line(3, 1, true, "Deckhand's Shirt")],
                ..plain_run()
            }],
        };
        let lua = render_runs_lua(&runs, 1).unwrap();
        assert!(!lua.contains("name ="));
        assert!(lua.contains("n = 'Deckhand\\'s Shirt'"));
        assert!(lua.contains("v = true"));
    }

    #[test]
    fn an_unparseable_updated_at_fails_the_whole_render_rather_than_lying_as_epoch() {
        let runs = WireRuns {
            v: 1,
            generated_at: "2026-09-15T10:00:00.000Z".into(),
            plan: "free".into(),
            free_lines: 5,
            groups: vec![],
            caps: vec![],
            runs: vec![WireRun {
                code: "abcd2345".into(),
                name: None,
                updated_at: "not-a-date".into(),
                lines: vec![line(3, 1, true, "Deckhand's Shirt")],
                ..plain_run()
            }],
        };
        let err = render_runs_lua(&runs, 1).unwrap_err();
        assert!(err.contains("abcd2345"));
    }

    #[test]
    fn the_wire_shape_deserialises_from_the_api_json() {
        let json = r#"{"v":1,"generatedAt":"2026-09-15T10:00:00.000Z","plan":"free","freeLines":5,"runs":[{"code":"abcd2345","name":null,"updatedAt":"2026-09-15T09:00:00.000Z","lines":[{"itemId":5,"qty":210,"vendor":false,"nameEn":"Plant Protein"}]}]}"#;
        let parsed: WireRuns = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.runs[0].lines[0].qty, 210);
    }

    #[test]
    fn a_run_missing_the_name_key_entirely_still_deserialises() {
        let json = r#"{"v":1,"generatedAt":"2026-09-15T10:00:00.000Z","plan":"free","freeLines":5,"runs":[{"code":"abcd2345","updatedAt":"2026-09-15T09:00:00.000Z","lines":[]}]}"#;
        let parsed: WireRuns = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.runs[0].name, None);
    }

    #[test]
    fn the_rendered_lua_loads_in_a_sandboxed_lua_vm() {
        use mlua::{Lua, LuaOptions, StdLib};

        let runs = WireRuns {
            v: 1,
            generated_at: "2026-09-15T10:00:00.000Z".into(),
            plan: "pro".into(),
            free_lines: 5,
            groups: vec![],
            caps: vec![],
            runs: vec![WireRun {
                code: "abcd2345".into(),
                name: Some("O'Brien's \\ Emporium".into()),
                updated_at: "2026-09-15T09:00:00.000Z".into(),
                lines: vec![line(5, 210, false, "Plant Protein")],
                ..plain_run()
            }],
        };
        let lua_source = render_runs_lua(&runs, 1_789_000_000).unwrap();

        // No standard library: this mirrors savedvars.rs's sandbox, since the
        // addon loads this same file into a live WoW Lua environment.
        let lua = Lua::new_with(StdLib::NONE, LuaOptions::default()).unwrap();
        lua.load(&lua_source).exec().unwrap();

        let v: u32 = lua.load("return GoldCap_AppRuns.v").eval().unwrap();
        assert_eq!(v, 1);
        let name: String = lua
            .load("return GoldCap_AppRuns.runs[1].lines[1].n")
            .eval()
            .unwrap();
        assert_eq!(name, "Plant Protein");
        let run_name: String = lua
            .load("return GoldCap_AppRuns.runs[1].name")
            .eval()
            .unwrap();
        assert_eq!(run_name, "O'Brien's \\ Emporium");

        // An empty-runs render also loads.
        let empty = WireRuns {
            v: 1,
            generated_at: "2026-09-15T10:00:00.000Z".into(),
            plan: "free".into(),
            free_lines: 5,
            groups: vec![],
            caps: vec![],
            runs: vec![],
        };
        let empty_source = render_runs_lua(&empty, 1).unwrap();
        let lua2 = Lua::new_with(StdLib::NONE, LuaOptions::default()).unwrap();
        lua2.load(&empty_source).exec().unwrap();
        let count: i64 = lua2.load("return #GoldCap_AppRuns.runs").eval().unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn a_fetch_error_leaves_the_previous_file_alone() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(crate::luafile::RUNS_FILE_NAME), "old").unwrap();
        let r = apply_fetch_result(dir.path(), Err("boom".into()), 1);
        assert!(r.is_err());
        assert_eq!(
            std::fs::read_to_string(dir.path().join(crate::luafile::RUNS_FILE_NAME)).unwrap(),
            "old"
        );
    }

    #[test]
    fn a_fetch_writes_the_file_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let runs = WireRuns {
            v: 1,
            generated_at: "2026-09-15T10:00:00.000Z".into(),
            plan: "free".into(),
            free_lines: 5,
            groups: vec![],
            caps: vec![],
            runs: vec![],
        };
        assert!(apply_fetch_result(dir.path(), Ok(runs), 7).unwrap());
        let s = std::fs::read_to_string(dir.path().join(crate::luafile::RUNS_FILE_NAME)).unwrap();
        assert!(s.starts_with("GoldCap_AppRuns = { v = 1, generatedAt = 7"));
        assert!(!dir.path().join("Runs.lua.tmp").exists());
    }

    #[test]
    fn an_unsupported_version_is_refused_before_writing() {
        let dir = tempfile::tempdir().unwrap();
        let runs = WireRuns {
            v: 4,
            generated_at: String::new(),
            plan: "free".into(),
            free_lines: 5,
            groups: vec![],
            caps: vec![],
            runs: vec![],
        };
        assert!(apply_fetch_result(dir.path(), Ok(runs), 7).is_err());
        assert!(!dir.path().join(crate::luafile::RUNS_FILE_NAME).exists());
    }

    #[test]
    fn a_v2_payload_is_written_with_its_own_version_and_prices() {
        let dir = tempfile::tempdir().unwrap();
        let runs = WireRuns {
            v: 2,
            generated_at: "2026-09-15T10:00:00.000Z".into(),
            plan: "pro".into(),
            free_lines: 5,
            groups: vec![],
            caps: vec![],
            runs: vec![WireRun {
                code: "abcd2345".into(),
                name: Some("Cooking 1-100".into()),
                updated_at: "2026-09-15T09:00:00.000Z".into(),
                lines: vec![WireRunLine {
                    usual: Some(450),
                    vendor_unit: Some(25),
                    cheap_hour: Some(WireCheapHour { hour: 3, pct: -18 }),
                    ..line(2589, 20, false, "Linen Cloth")
                }],
                ..plain_run()
            }],
        };
        assert!(apply_fetch_result(dir.path(), Ok(runs), 7).unwrap());
        let s = std::fs::read_to_string(dir.path().join(crate::luafile::RUNS_FILE_NAME)).unwrap();
        assert!(s.starts_with("GoldCap_AppRuns = { v = 2, generatedAt = 7"));
        assert!(s.contains("u = 450, vu = 25, ch = 3, cp = -18"));
    }

    #[test]
    fn a_v1_server_still_gets_its_file_written() {
        // A 1.10 companion in front of a site that has not shipped 2a yet.
        let dir = tempfile::tempdir().unwrap();
        let runs = WireRuns {
            v: 1,
            generated_at: "2026-09-15T10:00:00.000Z".into(),
            plan: "free".into(),
            free_lines: 5,
            groups: vec![],
            caps: vec![],
            runs: vec![WireRun {
                code: "abcd2345".into(),
                name: None,
                updated_at: "2026-09-15T09:00:00.000Z".into(),
                lines: vec![line(5, 210, false, "Plant Protein")],
                ..plain_run()
            }],
        };
        assert!(apply_fetch_result(dir.path(), Ok(runs), 7).unwrap());
        let s = std::fs::read_to_string(dir.path().join(crate::luafile::RUNS_FILE_NAME)).unwrap();
        assert!(s.starts_with("GoldCap_AppRuns = { v = 1, generatedAt = 7"));
        assert!(s.contains("n = 'Plant Protein' }"));
        assert!(!s.contains(", u = "));
    }

    #[test]
    fn an_unsupported_version_leaves_a_previously_written_file_alone() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(crate::luafile::RUNS_FILE_NAME), "old").unwrap();
        let runs = WireRuns {
            v: 99,
            generated_at: String::new(),
            plan: "free".into(),
            free_lines: 5,
            groups: vec![],
            caps: vec![],
            runs: vec![],
        };
        assert!(apply_fetch_result(dir.path(), Ok(runs), 7).is_err());
        assert_eq!(
            std::fs::read_to_string(dir.path().join(crate::luafile::RUNS_FILE_NAME)).unwrap(),
            "old"
        );
    }

    #[test]
    fn a_v1_payload_renders_without_any_of_the_new_fields() {
        // The site ships before the companion does, but the reverse also has
        // to hold: a 1.10 companion pointed at a v1 server must produce the
        // file phase 1 produced, byte for byte (the exact-string assertion
        // lives in renders_the_addon_table_shape; this names the contract).
        let runs = WireRuns {
            v: 1,
            generated_at: "2026-09-15T10:00:00.000Z".into(),
            plan: "pro".into(),
            free_lines: 5,
            groups: vec![],
            caps: vec![],
            runs: vec![WireRun {
                code: "abcd2345".into(),
                name: Some("Cooking 1-100".into()),
                updated_at: "2026-09-15T09:00:00.000Z".into(),
                lines: vec![line(5, 210, false, "Plant Protein")],
                ..plain_run()
            }],
        };
        let lua = render_runs_lua(&runs, 1_789_000_000).unwrap();
        assert!(lua.contains("n = 'Plant Protein' }"));
        for key in [
            ", u = ",
            ", vu = ",
            ", ch = ",
            ", cp = ",
            ", cc = ",
            ", rl = ",
            ", cr = ",
            ", minIlvl = ",
        ] {
            assert!(!lua.contains(key), "a v1 render leaked {key}");
        }
        for key in ["k = 'alert'", "by = '", "src = '"] {
            assert!(!lua.contains(key), "a v1 render leaked {key}");
        }
    }

    #[test]
    fn a_v2_line_renders_the_price_fields_in_order() {
        let runs = WireRuns {
            v: 2,
            generated_at: "2026-09-15T10:00:00.000Z".into(),
            plan: "pro".into(),
            free_lines: 5,
            groups: vec![],
            caps: vec![],
            runs: vec![WireRun {
                code: "abcd2345".into(),
                name: Some("Cooking 1-100".into()),
                updated_at: "2026-09-15T09:00:00.000Z".into(),
                lines: vec![WireRunLine {
                    usual: Some(450),
                    vendor_unit: Some(25),
                    cheap_hour: Some(WireCheapHour { hour: 3, pct: -18 }),
                    ..line(2589, 20, false, "Linen Cloth")
                }],
                ..plain_run()
            }],
        };
        let lua = render_runs_lua(&runs, 1_789_000_000).unwrap();
        let updated_at = crate::ledger_summary::parse_iso_utc("2026-09-15T09:00:00.000Z").unwrap();
        let expected = format!(
            "GoldCap_AppRuns = {{ v = 2, generatedAt = 1789000000, plan = 'pro', freeLines = 5, runs = {{ {{ code = 'abcd2345', name = 'Cooking 1-100', updatedAt = {updated_at}, lines = {{ {{ i = 2589, q = 20, v = false, n = 'Linen Cloth', u = 450, vu = 25, ch = 3, cp = -18 }} }} }} }} }}\n"
        );
        assert_eq!(lua, expected);
    }

    #[test]
    fn each_absent_price_field_is_omitted_rather_than_rendered_empty() {
        let render_one = |l: WireRunLine| -> String {
            let runs = WireRuns {
                v: 2,
                generated_at: "2026-09-15T10:00:00.000Z".into(),
                plan: "pro".into(),
                free_lines: 5,
                groups: vec![],
                caps: vec![],
                runs: vec![WireRun {
                    code: "abcd2345".into(),
                    name: None,
                    updated_at: "2026-09-15T09:00:00.000Z".into(),
                    lines: vec![l],
                    ..plain_run()
                }],
            };
            render_runs_lua(&runs, 1).unwrap()
        };

        let usual_only = render_one(WireRunLine {
            usual: Some(450),
            ..line(2589, 20, false, "Linen Cloth")
        });
        assert!(usual_only.contains("n = 'Linen Cloth', u = 450 }"));
        for key in [", vu = ", ", ch = ", ", cp = "] {
            assert!(!usual_only.contains(key), "leaked {key}");
        }

        let vendor_only = render_one(WireRunLine {
            vendor_unit: Some(25),
            ..line(159, 5, true, "Refreshing Spring Water")
        });
        assert!(vendor_only.contains("n = 'Refreshing Spring Water', vu = 25 }"));
        for key in [", u = ", ", ch = ", ", cp = "] {
            assert!(!vendor_only.contains(key), "leaked {key}");
        }

        let hour_only = render_one(WireRunLine {
            cheap_hour: Some(WireCheapHour { hour: 3, pct: -18 }),
            ..line(2589, 20, false, "Linen Cloth")
        });
        assert!(hour_only.contains("n = 'Linen Cloth', ch = 3, cp = -18 }"));
        for key in [", u = ", ", vu = "] {
            assert!(!hour_only.contains(key), "leaked {key}");
        }
    }

    #[test]
    fn a_zero_price_is_dropped_rather_than_rendered() {
        // The site stores an unknown vendor price as NULL, so a 0 would price a
        // line at nothing in game. Absent beats misleading.
        let runs = WireRuns {
            v: 2,
            generated_at: "2026-09-15T10:00:00.000Z".into(),
            plan: "pro".into(),
            free_lines: 5,
            groups: vec![],
            caps: vec![],
            runs: vec![WireRun {
                code: "abcd2345".into(),
                name: None,
                updated_at: "2026-09-15T09:00:00.000Z".into(),
                lines: vec![WireRunLine {
                    usual: Some(0),
                    vendor_unit: Some(0),
                    ..line(2589, 20, false, "Linen Cloth")
                }],
                ..plain_run()
            }],
        };
        let lua = render_runs_lua(&runs, 1).unwrap();
        assert!(!lua.contains(", u = "));
        assert!(!lua.contains(", vu = "));
    }

    #[test]
    fn a_cheap_hour_outside_the_clock_is_dropped_rather_than_rendered() {
        // `hour` is a clock position; 25 cannot be shown as one. Same
        // all-or-nothing rule savedvars.rs applies to decision evidence: an
        // out-of-contract value is absent, not misleading.
        let runs = WireRuns {
            v: 2,
            generated_at: "2026-09-15T10:00:00.000Z".into(),
            plan: "pro".into(),
            free_lines: 5,
            groups: vec![],
            caps: vec![],
            runs: vec![WireRun {
                code: "abcd2345".into(),
                name: None,
                updated_at: "2026-09-15T09:00:00.000Z".into(),
                lines: vec![WireRunLine {
                    cheap_hour: Some(WireCheapHour { hour: 25, pct: -18 }),
                    ..line(2589, 20, false, "Linen Cloth")
                }],
                ..plain_run()
            }],
        };
        let lua = render_runs_lua(&runs, 1).unwrap();
        assert!(!lua.contains(", ch = "));
        assert!(!lua.contains(", cp = "));
    }

    #[test]
    fn a_v2_json_payload_carries_the_price_fields() {
        let json = r#"{"v":2,"generatedAt":"2026-09-15T10:00:00.000Z","plan":"pro","freeLines":5,"runs":[{"code":"abcd2345","name":"Cooking 1-100","updatedAt":"2026-09-15T09:00:00.000Z","lines":[{"itemId":2589,"qty":20,"vendor":false,"nameEn":"Linen Cloth","usual":450,"vendorUnit":25,"cheapHour":{"hour":3,"pct":-18}}]}]}"#;
        let parsed: WireRuns = serde_json::from_str(json).unwrap();
        let l = &parsed.runs[0].lines[0];
        assert_eq!(l.usual, Some(450));
        assert_eq!(l.vendor_unit, Some(25));
        assert_eq!(l.cheap_hour, Some(WireCheapHour { hour: 3, pct: -18 }));
    }

    #[test]
    fn a_v1_json_payload_and_explicit_nulls_both_read_as_absent() {
        let v1 = r#"{"v":1,"generatedAt":"2026-09-15T10:00:00.000Z","plan":"free","freeLines":5,"runs":[{"code":"abcd2345","name":null,"updatedAt":"2026-09-15T09:00:00.000Z","lines":[{"itemId":5,"qty":210,"vendor":false,"nameEn":"Plant Protein"}]}]}"#;
        let parsed: WireRuns = serde_json::from_str(v1).unwrap();
        let l = &parsed.runs[0].lines[0];
        assert_eq!(l.usual, None);
        assert_eq!(l.vendor_unit, None);
        assert_eq!(l.cheap_hour, None);

        let nulls = r#"{"v":2,"generatedAt":"2026-09-15T10:00:00.000Z","plan":"free","freeLines":5,"runs":[{"code":"abcd2345","name":null,"updatedAt":"2026-09-15T09:00:00.000Z","lines":[{"itemId":5,"qty":210,"vendor":false,"nameEn":"Plant Protein","usual":null,"vendorUnit":null,"cheapHour":null}]}]}"#;
        let parsed: WireRuns = serde_json::from_str(nulls).unwrap();
        let l = &parsed.runs[0].lines[0];
        assert_eq!(l.usual, None);
        assert_eq!(l.vendor_unit, None);
        assert_eq!(l.cheap_hour, None);
    }

    #[test]
    fn a_fractional_number_rounds_instead_of_failing_the_whole_payload() {
        // The reference price can come out of a median, and a percentage out
        // of an average: a single `.5` must not make the whole response
        // undeserialisable, because that would strand Runs.lua at whatever it
        // said yesterday, silently, on every tick from then on.
        let json = r#"{"v":2,"generatedAt":"2026-09-15T10:00:00.000Z","plan":"pro","freeLines":5,"runs":[{"code":"abcd2345","name":null,"updatedAt":"2026-09-15T09:00:00.000Z","lines":[{"itemId":2589,"qty":20,"vendor":false,"nameEn":"Linen Cloth","usual":450.5,"vendorUnit":25.0,"cheapHour":{"hour":3.0,"pct":-18.0}}]}]}"#;
        let parsed: WireRuns = serde_json::from_str(json).unwrap();
        let l = &parsed.runs[0].lines[0];
        assert_eq!(l.usual, Some(451));
        assert_eq!(l.vendor_unit, Some(25));
        assert_eq!(l.cheap_hour, Some(WireCheapHour { hour: 3, pct: -18 }));
    }

    #[test]
    fn a_v2_render_loads_in_a_sandboxed_lua_vm_with_the_new_fields_readable() {
        use mlua::{Lua, LuaOptions, StdLib};

        let runs = WireRuns {
            v: 2,
            generated_at: "2026-09-15T10:00:00.000Z".into(),
            plan: "pro".into(),
            free_lines: 5,
            groups: vec![],
            caps: vec![],
            runs: vec![WireRun {
                code: "abcd2345".into(),
                name: Some("Cooking 1-100".into()),
                updated_at: "2026-09-15T09:00:00.000Z".into(),
                lines: vec![
                    WireRunLine {
                        usual: Some(450),
                        vendor_unit: Some(25),
                        cheap_hour: Some(WireCheapHour { hour: 3, pct: -18 }),
                        ..line(2589, 20, false, "Linen Cloth")
                    },
                    line(159, 5, true, "Refreshing Spring Water"),
                ],
                ..plain_run()
            }],
        };
        let lua_source = render_runs_lua(&runs, 1_789_000_000).unwrap();

        let lua = Lua::new_with(StdLib::NONE, LuaOptions::default()).unwrap();
        lua.load(&lua_source).exec().unwrap();

        let v: u32 = lua.load("return GoldCap_AppRuns.v").eval().unwrap();
        assert_eq!(v, 2);
        let u: i64 = lua
            .load("return GoldCap_AppRuns.runs[1].lines[1].u")
            .eval()
            .unwrap();
        assert_eq!(u, 450);
        let vu: i64 = lua
            .load("return GoldCap_AppRuns.runs[1].lines[1].vu")
            .eval()
            .unwrap();
        assert_eq!(vu, 25);
        let ch: i64 = lua
            .load("return GoldCap_AppRuns.runs[1].lines[1].ch")
            .eval()
            .unwrap();
        assert_eq!(ch, 3);
        let cp: i64 = lua
            .load("return GoldCap_AppRuns.runs[1].lines[1].cp")
            .eval()
            .unwrap();
        assert_eq!(cp, -18);

        // A line the site had nothing to say about reads as nil in game.
        let missing: Option<i64> = lua
            .load("return GoldCap_AppRuns.runs[1].lines[2].u")
            .eval()
            .unwrap();
        assert_eq!(missing, None);
    }

    #[test]
    fn a_v3_json_payload_carries_the_run_and_line_facts() {
        let json = r#"{"v":3,"generatedAt":"2026-09-15T10:00:00.000Z","plan":"pro","freeLines":5,"runs":[{"code":"a0000016","name":"Flask watch","updatedAt":"2026-09-15T09:00:00.000Z","kind":"alert","sharedBy":"Acromion","source":{"kind":"profession","label":"Cooking 1-100"},"lines":[{"itemId":212264,"qty":3,"vendor":false,"nameEn":"Flask of Alchemical Chaos","usual":120000,"cap":99900,"realm":{"id":1305,"name":"Kazzak"},"craft":{"recipeId":9876,"craftedQty":5,"costCopper":2300,"reagents":[{"itemId":224060,"qty":5,"nameEn":"Eversong Trout","vendor":false,"usual":400,"vendorUnit":null},{"itemId":2678,"qty":5,"nameEn":"Tavern Fixings","vendor":true,"usual":null,"vendorUnit":60}]}}]}]}"#;
        let parsed: WireRuns = serde_json::from_str(json).unwrap();
        let run = &parsed.runs[0];
        assert_eq!(run.kind.as_deref(), Some("alert"));
        assert_eq!(run.shared_by.as_deref(), Some("Acromion"));
        assert_eq!(
            run.source.as_ref().map(|s| s.label.as_str()),
            Some("Cooking 1-100")
        );
        let l = &run.lines[0];
        assert_eq!(l.cap, Some(99900));
        assert_eq!(
            l.realm,
            Some(WireRealm {
                id: 1305,
                name: "Kazzak".into()
            })
        );
        let craft = l.craft.as_ref().unwrap();
        assert_eq!(craft.recipe_id, 9876);
        assert_eq!(craft.crafted_qty, 5);
        assert_eq!(craft.cost_copper, 2300);
        assert_eq!(craft.reagents.len(), 2);
        assert_eq!(craft.reagents[0].item_id, 224060);
        assert_eq!(craft.reagents[0].qty, 5);
        assert_eq!(craft.reagents[0].name_en, "Eversong Trout");
        assert!(!craft.reagents[0].vendor);
        assert_eq!(craft.reagents[0].usual, Some(400));
        assert_eq!(craft.reagents[0].vendor_unit, None);
        assert!(craft.reagents[1].vendor);
        assert_eq!(craft.reagents[1].usual, None);
        assert_eq!(craft.reagents[1].vendor_unit, Some(60));
    }

    #[test]
    fn a_malformed_new_field_reads_as_absent_instead_of_failing_the_sync() {
        // Every one of these would otherwise fail the whole response — and a
        // failed response writes nothing, so Runs.lua would sit at yesterday's
        // file silently, on this tick and every tick after it. One field the
        // companion cannot read costs its own fact and nothing else.
        let json = r#"{"v":3,"generatedAt":"2026-09-15T10:00:00.000Z","plan":"pro","freeLines":5,"runs":[{"code":"abcd2345","name":null,"updatedAt":"2026-09-15T09:00:00.000Z","kind":7,"sharedBy":{"name":"Acromion"},"source":{"kind":"profession"},"lines":[{"itemId":2589,"qty":20,"vendor":false,"nameEn":"Linen Cloth","cap":"99900","realm":"Kazzak","craft":{"recipeId":9876,"craftedQty":5}}]}]}"#;
        let parsed: WireRuns = serde_json::from_str(json).unwrap();
        let run = &parsed.runs[0];
        assert_eq!(run.kind, None, "a number is not a kind");
        assert_eq!(run.shared_by, None, "an object is not a name");
        assert_eq!(run.source, None, "a source without a label says nothing");
        let l = &run.lines[0];
        assert_eq!(l.cap, None, "a string is not copper");
        assert_eq!(l.realm, None, "a string is not a realm");
        assert_eq!(l.craft, None, "a craft without reagents cannot be split");
        // The line itself is untouched by any of it.
        assert_eq!(l.item_id, 2589);
        assert_eq!(l.qty, 20);
        assert_eq!(l.name_en, "Linen Cloth");
    }

    #[test]
    fn a_malformed_v2_field_inside_a_v3_payload_reads_as_absent_too() {
        // v2's `usual`/`vendorUnit`/`cheapHour`, a reagent's own `usual`, and
        // the run's `name` used to be read strictly, so a v3 payload with one
        // bad v2-era value failed the whole `WireRuns` parse — and a failed
        // parse writes nothing, stranding Runs.lua exactly like the v3-only
        // fields the test above guards against.
        let json = r#"{"v":3,"generatedAt":"2026-09-15T10:00:00.000Z","plan":"pro","freeLines":5,"runs":[{"code":"abcd2345","name":5,"updatedAt":"2026-09-15T09:00:00.000Z","lines":[{"itemId":2589,"qty":20,"vendor":false,"nameEn":"Linen Cloth","usual":"450","vendorUnit":"25","cheapHour":3,"craft":{"recipeId":9876,"craftedQty":5,"costCopper":2300,"reagents":[{"itemId":224060,"qty":5,"nameEn":"Eversong Trout","vendor":false,"usual":"400","vendorUnit":"60"}]}}]}]}"#;
        let parsed: WireRuns = serde_json::from_str(json).unwrap();
        let run = &parsed.runs[0];
        assert_eq!(run.name, None, "a number is not a run name");
        let l = &run.lines[0];
        assert_eq!(l.usual, None, "a string is not copper");
        assert_eq!(l.vendor_unit, None, "a string is not copper");
        assert_eq!(l.cheap_hour, None, "a number is not a cheap-hour object");
        let reagent = &l.craft.as_ref().unwrap().reagents[0];
        assert_eq!(reagent.usual, None, "a string is not copper");
        assert_eq!(reagent.vendor_unit, None, "a string is not copper");
        // The line's other facts survive the bad neighbours untouched.
        assert_eq!(l.item_id, 2589);
        assert_eq!(l.qty, 20);
        assert_eq!(l.name_en, "Linen Cloth");

        let lua = render_runs_lua(&parsed, 1).unwrap();
        assert!(!lua.contains(", u = "), "rendered usual from a bad string");
        assert!(
            !lua.contains(", vu = "),
            "rendered vendor_unit from a bad string"
        );
        assert!(
            !lua.contains(", ch = "),
            "rendered cheap_hour from a bad shape"
        );
        assert!(!lua.contains(", cp = "), "rendered cheap_hour's pct");
        assert!(
            !lua.contains("name = "),
            "rendered a name from a bad number"
        );
    }

    #[test]
    fn a_v3_field_this_build_has_never_heard_of_is_ignored() {
        // Additions only: a later contract must not strand this build.
        let json = r#"{"v":3,"generatedAt":"2026-09-15T10:00:00.000Z","plan":"pro","freeLines":5,"mystery":true,"runs":[{"code":"abcd2345","name":null,"updatedAt":"2026-09-15T09:00:00.000Z","futureRunField":{"a":1},"lines":[{"itemId":2589,"qty":20,"vendor":false,"nameEn":"Linen Cloth","futureLineField":[1,2,3]}]}]}"#;
        let parsed: WireRuns = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.runs[0].lines[0].item_id, 2589);
    }

    #[test]
    fn a_v1_or_v2_payload_reads_the_v3_fields_as_absent() {
        let v1 = r#"{"v":1,"generatedAt":"2026-09-15T10:00:00.000Z","plan":"free","freeLines":5,"runs":[{"code":"abcd2345","name":null,"updatedAt":"2026-09-15T09:00:00.000Z","lines":[{"itemId":5,"qty":210,"vendor":false,"nameEn":"Plant Protein"}]}]}"#;
        let parsed: WireRuns = serde_json::from_str(v1).unwrap();
        assert_eq!(parsed.runs[0].kind, None);
        assert_eq!(parsed.runs[0].shared_by, None);
        assert_eq!(parsed.runs[0].source, None);
        assert_eq!(parsed.runs[0].lines[0].cap, None);
        assert_eq!(parsed.runs[0].lines[0].realm, None);
        assert_eq!(parsed.runs[0].lines[0].craft, None);

        let nulls = r#"{"v":3,"generatedAt":"2026-09-15T10:00:00.000Z","plan":"free","freeLines":5,"runs":[{"code":"abcd2345","name":null,"updatedAt":"2026-09-15T09:00:00.000Z","kind":null,"sharedBy":null,"source":null,"lines":[{"itemId":5,"qty":210,"vendor":false,"nameEn":"Plant Protein","cap":null,"realm":null,"craft":null}]}]}"#;
        let parsed: WireRuns = serde_json::from_str(nulls).unwrap();
        assert_eq!(parsed.runs[0].kind, None);
        assert_eq!(parsed.runs[0].shared_by, None);
        assert_eq!(parsed.runs[0].source, None);
        assert_eq!(parsed.runs[0].lines[0].cap, None);
        assert_eq!(parsed.runs[0].lines[0].realm, None);
        assert_eq!(parsed.runs[0].lines[0].craft, None);
    }

    #[test]
    fn a_fractional_cap_or_craft_cost_rounds_instead_of_dropping_the_fact() {
        // Same reason phase 2a rounds a price: these come out of medians, and
        // one `.5` must cost nothing at all.
        let json = r#"{"v":3,"generatedAt":"2026-09-15T10:00:00.000Z","plan":"pro","freeLines":5,"runs":[{"code":"abcd2345","name":null,"updatedAt":"2026-09-15T09:00:00.000Z","lines":[{"itemId":2589,"qty":20,"vendor":false,"nameEn":"Linen Cloth","cap":99900.5,"craft":{"recipeId":9876,"craftedQty":5,"costCopper":2300.4,"reagents":[{"itemId":224060,"qty":5,"nameEn":"Eversong Trout","vendor":false,"usual":400.5,"vendorUnit":null}]}}]}]}"#;
        let parsed: WireRuns = serde_json::from_str(json).unwrap();
        let l = &parsed.runs[0].lines[0];
        assert_eq!(l.cap, Some(99901));
        let craft = l.craft.as_ref().unwrap();
        assert_eq!(craft.cost_copper, 2300);
        assert_eq!(craft.reagents[0].usual, Some(401));
    }

    #[test]
    fn a_v3_alert_run_renders_its_kind_cap_and_realm() {
        let runs = WireRuns {
            v: 3,
            generated_at: "2026-09-15T10:00:00.000Z".into(),
            plan: "pro".into(),
            free_lines: 5,
            groups: vec![],
            caps: vec![],
            runs: vec![WireRun {
                code: "a0000016".into(),
                name: Some("Flask watch".into()),
                updated_at: "2026-09-15T09:00:00.000Z".into(),
                kind: Some("alert".into()),
                lines: vec![WireRunLine {
                    usual: Some(120_000),
                    cap: Some(99_900),
                    realm: Some(WireRealm {
                        id: 1305,
                        name: "Kazzak".into(),
                    }),
                    ..line(212264, 3, false, "Flask of Alchemical Chaos")
                }],
                ..plain_run()
            }],
        };
        let lua = render_runs_lua(&runs, 1_789_000_000).unwrap();
        let updated_at = crate::ledger_summary::parse_iso_utc("2026-09-15T09:00:00.000Z").unwrap();
        let expected = format!(
            "GoldCap_AppRuns = {{ v = 3, generatedAt = 1789000000, plan = 'pro', freeLines = 5, runs = {{ {{ code = 'a0000016', name = 'Flask watch', k = 'alert', updatedAt = {updated_at}, lines = {{ {{ i = 212264, q = 3, v = false, n = 'Flask of Alchemical Chaos', u = 120000, cc = 99900, rl = {{ id = 1305, n = 'Kazzak' }} }} }} }} }} }}\n"
        );
        assert_eq!(lua, expected);
    }

    #[test]
    fn a_v3_followed_run_renders_by_src_and_the_craft() {
        let runs = WireRuns {
            v: 3,
            generated_at: "2026-09-15T10:00:00.000Z".into(),
            plan: "pro".into(),
            free_lines: 5,
            groups: vec![],
            caps: vec![],
            runs: vec![WireRun {
                code: "abcd2345".into(),
                name: Some("Cooking 1-100".into()),
                updated_at: "2026-09-15T09:00:00.000Z".into(),
                shared_by: Some("Acromion".into()),
                source: Some(WireRunSource {
                    label: "Cooking 1-100".into(),
                }),
                lines: vec![WireRunLine {
                    craft: Some(WireCraft {
                        recipe_id: 9876,
                        crafted_qty: 5,
                        cost_copper: 2300,
                        reagents: vec![
                            WireCraftReagent {
                                item_id: 224060,
                                qty: 5,
                                name_en: "Eversong Trout".into(),
                                vendor: false,
                                usual: Some(400),
                                vendor_unit: None,
                            },
                            WireCraftReagent {
                                item_id: 2678,
                                qty: 5,
                                name_en: "Tavern Fixings".into(),
                                vendor: true,
                                usual: None,
                                vendor_unit: Some(60),
                            },
                        ],
                    }),
                    ..line(222724, 461, false, "Thalassian Fillet")
                }],
                ..plain_run()
            }],
        };
        let lua = render_runs_lua(&runs, 1_789_000_000).unwrap();
        let updated_at = crate::ledger_summary::parse_iso_utc("2026-09-15T09:00:00.000Z").unwrap();
        let expected = format!(
            "GoldCap_AppRuns = {{ v = 3, generatedAt = 1789000000, plan = 'pro', freeLines = 5, runs = {{ {{ code = 'abcd2345', name = 'Cooking 1-100', by = 'Acromion', src = 'Cooking 1-100', updatedAt = {updated_at}, lines = {{ {{ i = 222724, q = 461, v = false, n = 'Thalassian Fillet', cr = {{ r = 9876, n = 5, c = 2300, i = {{ {{ i = 224060, q = 5, n = 'Eversong Trout', v = false, u = 400 }}, {{ i = 2678, q = 5, n = 'Tavern Fixings', v = true, vu = 60 }} }} }} }} }} }} }} }}\n"
        );
        assert_eq!(lua, expected);
    }

    #[test]
    fn a_list_run_never_writes_the_default_kind() {
        // Absent `k` is what v1 and v2 always meant; writing it would be a
        // key in every table in the file that says nothing.
        for kind in [None, Some("list"), Some("mystery")] {
            let runs = WireRuns {
                v: 3,
                generated_at: "2026-09-15T10:00:00.000Z".into(),
                plan: "pro".into(),
                free_lines: 5,
                groups: vec![],
                caps: vec![],
                runs: vec![WireRun {
                    kind: kind.map(Into::into),
                    lines: vec![line(5, 210, false, "Plant Protein")],
                    ..plain_run()
                }],
            };
            let lua = render_runs_lua(&runs, 1).unwrap();
            assert!(!lua.contains("k = "), "wrote a kind for {kind:?}");
        }
    }

    #[test]
    fn a_blank_shared_name_or_source_label_is_dropped_rather_than_rendered_empty() {
        // `by = ''` would read in game as "from" with a hole after it.
        let runs = WireRuns {
            v: 3,
            generated_at: "2026-09-15T10:00:00.000Z".into(),
            plan: "pro".into(),
            free_lines: 5,
            groups: vec![],
            caps: vec![],
            runs: vec![WireRun {
                shared_by: Some("   ".into()),
                source: Some(WireRunSource { label: "".into() }),
                lines: vec![line(5, 210, false, "Plant Protein")],
                ..plain_run()
            }],
        };
        let lua = render_runs_lua(&runs, 1).unwrap();
        assert!(!lua.contains("by = "));
        assert!(!lua.contains("src = "));
    }

    #[test]
    fn a_cap_of_zero_or_less_is_dropped_rather_than_rendered() {
        // A ceiling of nothing would buy nothing; absent means "use the
        // addon's own cap%", which is the behaviour every v2 line has.
        for cap in [Some(0), Some(-1)] {
            let runs = WireRuns {
                v: 3,
                generated_at: "2026-09-15T10:00:00.000Z".into(),
                plan: "pro".into(),
                free_lines: 5,
                groups: vec![],
                caps: vec![],
                runs: vec![WireRun {
                    lines: vec![WireRunLine {
                        cap,
                        ..line(2589, 20, false, "Linen Cloth")
                    }],
                    ..plain_run()
                }],
            };
            let lua = render_runs_lua(&runs, 1).unwrap();
            assert!(!lua.contains(", cc = "), "wrote a cap of {cap:?}");
        }
    }

    #[test]
    fn a_realm_the_addon_could_not_use_is_dropped_whole() {
        for realm in [
            WireRealm {
                id: 0,
                name: "Kazzak".into(),
            },
            WireRealm {
                id: 1305,
                name: "  ".into(),
            },
        ] {
            let runs = WireRuns {
                v: 3,
                generated_at: "2026-09-15T10:00:00.000Z".into(),
                plan: "pro".into(),
                free_lines: 5,
                groups: vec![],
                caps: vec![],
                runs: vec![WireRun {
                    lines: vec![WireRunLine {
                        realm: Some(realm.clone()),
                        ..line(2589, 20, false, "Linen Cloth")
                    }],
                    ..plain_run()
                }],
            };
            let lua = render_runs_lua(&runs, 1).unwrap();
            assert!(!lua.contains(", rl = "), "wrote realm {realm:?}");
        }
    }

    #[test]
    fn a_craft_the_addon_could_not_act_on_is_dropped_whole() {
        // Each of these is something the addon would have to do the
        // impossible with: name no recipe, divide a need by a yield of zero,
        // compare against a cost of nothing, split into no reagents.
        let good = WireCraft {
            recipe_id: 9876,
            crafted_qty: 5,
            cost_copper: 2300,
            reagents: vec![WireCraftReagent {
                item_id: 224060,
                qty: 5,
                name_en: "Eversong Trout".into(),
                vendor: false,
                usual: Some(400),
                vendor_unit: None,
            }],
        };
        let broken = [
            WireCraft {
                recipe_id: 0,
                ..good.clone()
            },
            WireCraft {
                crafted_qty: 0,
                ..good.clone()
            },
            WireCraft {
                cost_copper: 0,
                ..good.clone()
            },
            WireCraft {
                reagents: vec![],
                ..good.clone()
            },
            WireCraft {
                reagents: vec![WireCraftReagent {
                    item_id: 0,
                    ..good.reagents[0].clone()
                }],
                ..good.clone()
            },
            WireCraft {
                reagents: vec![WireCraftReagent {
                    qty: 0,
                    ..good.reagents[0].clone()
                }],
                ..good.clone()
            },
        ];
        for craft in broken {
            let runs = WireRuns {
                v: 3,
                generated_at: "2026-09-15T10:00:00.000Z".into(),
                plan: "pro".into(),
                free_lines: 5,
                groups: vec![],
                caps: vec![],
                runs: vec![WireRun {
                    lines: vec![WireRunLine {
                        craft: Some(craft.clone()),
                        ..line(222724, 461, false, "Thalassian Fillet")
                    }],
                    ..plain_run()
                }],
            };
            let lua = render_runs_lua(&runs, 1).unwrap();
            assert!(!lua.contains(", cr = "), "wrote craft {craft:?}");
        }

        // …and the good one still renders, so the guards are not a blanket.
        let runs = WireRuns {
            v: 3,
            generated_at: "2026-09-15T10:00:00.000Z".into(),
            plan: "pro".into(),
            free_lines: 5,
            groups: vec![],
            caps: vec![],
            runs: vec![WireRun {
                lines: vec![WireRunLine {
                    craft: Some(good),
                    ..line(222724, 461, false, "Thalassian Fillet")
                }],
                ..plain_run()
            }],
        };
        let lua = render_runs_lua(&runs, 1).unwrap();
        assert!(lua.contains(", cr = { r = 9876, n = 5, c = 2300, i = { "));
    }

    #[test]
    fn a_reagents_unknown_price_is_omitted_the_way_a_lines_is() {
        let runs = WireRuns {
            v: 3,
            generated_at: "2026-09-15T10:00:00.000Z".into(),
            plan: "pro".into(),
            free_lines: 5,
            groups: vec![],
            caps: vec![],
            runs: vec![WireRun {
                lines: vec![WireRunLine {
                    craft: Some(WireCraft {
                        recipe_id: 9876,
                        crafted_qty: 5,
                        cost_copper: 2300,
                        reagents: vec![WireCraftReagent {
                            item_id: 224060,
                            qty: 5,
                            name_en: "Eversong Trout".into(),
                            vendor: false,
                            usual: Some(0),
                            vendor_unit: Some(0),
                        }],
                    }),
                    ..line(222724, 461, false, "Thalassian Fillet")
                }],
                ..plain_run()
            }],
        };
        let lua = render_runs_lua(&runs, 1).unwrap();
        assert!(lua.contains("{ i = 224060, q = 5, n = 'Eversong Trout', v = false }"));
    }

    #[test]
    fn the_new_strings_are_escaped_like_every_other_name() {
        let runs = WireRuns {
            v: 3,
            generated_at: "2026-09-15T10:00:00.000Z".into(),
            plan: "pro".into(),
            free_lines: 5,
            groups: vec![],
            caps: vec![],
            runs: vec![WireRun {
                shared_by: Some("O'Brien".into()),
                source: Some(WireRunSource {
                    label: "Cooking's 1-100".into(),
                }),
                lines: vec![WireRunLine {
                    realm: Some(WireRealm {
                        id: 1305,
                        name: "Cho'gall".into(),
                    }),
                    craft: Some(WireCraft {
                        recipe_id: 9876,
                        crafted_qty: 5,
                        cost_copper: 2300,
                        reagents: vec![WireCraftReagent {
                            item_id: 3,
                            qty: 1,
                            name_en: "Deckhand's Shirt".into(),
                            vendor: true,
                            usual: None,
                            vendor_unit: Some(60),
                        }],
                    }),
                    ..line(222724, 461, false, "Thalassian Fillet")
                }],
                ..plain_run()
            }],
        };
        let lua = render_runs_lua(&runs, 1).unwrap();
        assert!(lua.contains("by = 'O\\'Brien'"));
        assert!(lua.contains("src = 'Cooking\\'s 1-100'"));
        assert!(lua.contains("n = 'Cho\\'gall'"));
        assert!(lua.contains("n = 'Deckhand\\'s Shirt'"));
    }

    #[test]
    fn a_v3_payload_is_written_with_its_own_version() {
        let dir = tempfile::tempdir().unwrap();
        let runs = WireRuns {
            v: 3,
            generated_at: "2026-09-15T10:00:00.000Z".into(),
            plan: "pro".into(),
            free_lines: 5,
            groups: vec![],
            caps: vec![],
            runs: vec![WireRun {
                code: "a0000016".into(),
                name: Some("Flask watch".into()),
                kind: Some("alert".into()),
                lines: vec![WireRunLine {
                    cap: Some(99_900),
                    ..line(212264, 3, false, "Flask of Alchemical Chaos")
                }],
                ..plain_run()
            }],
        };
        assert!(apply_fetch_result(dir.path(), Ok(runs), 7).unwrap());
        let s = std::fs::read_to_string(dir.path().join(crate::luafile::RUNS_FILE_NAME)).unwrap();
        assert!(s.starts_with("GoldCap_AppRuns = { v = 3, generatedAt = 7"));
        assert!(s.contains("k = 'alert'"));
        assert!(s.contains(", cc = 99900"));
    }

    #[test]
    fn a_v3_render_loads_in_a_sandboxed_lua_vm_with_the_new_fields_readable() {
        use mlua::{Lua, LuaOptions, StdLib};

        let runs = WireRuns {
            v: 3,
            generated_at: "2026-09-15T10:00:00.000Z".into(),
            plan: "pro".into(),
            free_lines: 5,
            groups: vec![],
            caps: vec![],
            runs: vec![
                WireRun {
                    code: "a0000016".into(),
                    name: Some("Flask watch".into()),
                    kind: Some("alert".into()),
                    lines: vec![WireRunLine {
                        cap: Some(99_900),
                        realm: Some(WireRealm {
                            id: 1305,
                            name: "Kazzak".into(),
                        }),
                        ..line(212264, 3, false, "Flask of Alchemical Chaos")
                    }],
                    ..plain_run()
                },
                WireRun {
                    code: "abcd2345".into(),
                    name: Some("Cooking 1-100".into()),
                    shared_by: Some("Acromion".into()),
                    source: Some(WireRunSource {
                        label: "Cooking 1-100".into(),
                    }),
                    lines: vec![WireRunLine {
                        craft: Some(WireCraft {
                            recipe_id: 9876,
                            crafted_qty: 5,
                            cost_copper: 2300,
                            reagents: vec![WireCraftReagent {
                                item_id: 224060,
                                qty: 5,
                                name_en: "Eversong Trout".into(),
                                vendor: false,
                                usual: Some(400),
                                vendor_unit: None,
                            }],
                        }),
                        ..line(222724, 461, false, "Thalassian Fillet")
                    }],
                    ..plain_run()
                },
            ],
        };
        let lua_source = render_runs_lua(&runs, 1_789_000_000).unwrap();

        let lua = Lua::new_with(StdLib::NONE, LuaOptions::default()).unwrap();
        lua.load(&lua_source).exec().unwrap();

        let v: u32 = lua.load("return GoldCap_AppRuns.v").eval().unwrap();
        assert_eq!(v, 3);
        let k: String = lua.load("return GoldCap_AppRuns.runs[1].k").eval().unwrap();
        assert_eq!(k, "alert");
        let cc: i64 = lua
            .load("return GoldCap_AppRuns.runs[1].lines[1].cc")
            .eval()
            .unwrap();
        assert_eq!(cc, 99_900);
        let realm_id: i64 = lua
            .load("return GoldCap_AppRuns.runs[1].lines[1].rl.id")
            .eval()
            .unwrap();
        assert_eq!(realm_id, 1305);
        let realm_name: String = lua
            .load("return GoldCap_AppRuns.runs[1].lines[1].rl.n")
            .eval()
            .unwrap();
        assert_eq!(realm_name, "Kazzak");

        let by: String = lua
            .load("return GoldCap_AppRuns.runs[2].by")
            .eval()
            .unwrap();
        assert_eq!(by, "Acromion");
        let src: String = lua
            .load("return GoldCap_AppRuns.runs[2].src")
            .eval()
            .unwrap();
        assert_eq!(src, "Cooking 1-100");
        let recipe: i64 = lua
            .load("return GoldCap_AppRuns.runs[2].lines[1].cr.r")
            .eval()
            .unwrap();
        assert_eq!(recipe, 9876);
        let crafted_qty: i64 = lua
            .load("return GoldCap_AppRuns.runs[2].lines[1].cr.n")
            .eval()
            .unwrap();
        assert_eq!(crafted_qty, 5);
        let cost: i64 = lua
            .load("return GoldCap_AppRuns.runs[2].lines[1].cr.c")
            .eval()
            .unwrap();
        assert_eq!(cost, 2300);
        let reagent_count: i64 = lua
            .load("return #GoldCap_AppRuns.runs[2].lines[1].cr.i")
            .eval()
            .unwrap();
        assert_eq!(reagent_count, 1);
        let reagent_name: String = lua
            .load("return GoldCap_AppRuns.runs[2].lines[1].cr.i[1].n")
            .eval()
            .unwrap();
        assert_eq!(reagent_name, "Eversong Trout");
        let reagent_usual: i64 = lua
            .load("return GoldCap_AppRuns.runs[2].lines[1].cr.i[1].u")
            .eval()
            .unwrap();
        assert_eq!(reagent_usual, 400);

        // The alert run has no craft and the followed run has no cap: both
        // read as nil in game rather than as a zero.
        let missing: Option<i64> = lua
            .load("return GoldCap_AppRuns.runs[2].lines[1].cc")
            .eval()
            .unwrap();
        assert_eq!(missing, None);
    }

    #[test]
    fn a_v3_payload_carries_groups_and_caps() {
        let json = r#"{"v":3,"generatedAt":"2026-09-22T10:00:00.000Z","plan":"pro","freeLines":5,"runs":[],"groups":["Transmog","Ore"],"caps":[{"i":212345,"c":1500000,"l":610,"g":0,"m":true},{"i":190311,"c":4200,"l":0,"g":1,"m":false}],"capsDropped":0}"#;
        let parsed: WireRuns = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.groups, vec!["Transmog".to_string(), "Ore".to_string()]);
        assert_eq!(parsed.caps.len(), 2);
        assert_eq!(parsed.caps[0], WireCap { item_id: 212345, cap: 1_500_000, min_ilvl: 610, group: Some(0), manual: true });
        assert!(!parsed.caps[1].manual);
    }

    #[test]
    fn a_payload_without_caps_reads_as_empty_and_renders_nothing_new() {
        let json = r#"{"v":3,"generatedAt":"2026-09-22T10:00:00.000Z","plan":"free","freeLines":5,"runs":[]}"#;
        let parsed: WireRuns = serde_json::from_str(json).unwrap();
        assert!(parsed.groups.is_empty() && parsed.caps.is_empty());
        let lua = render_runs_lua(&parsed, 1_700_000_000).unwrap();
        assert!(!lua.contains("caps") && !lua.contains("groups"));
        assert_eq!(lua, "GoldCap_AppRuns = { v = 3, generatedAt = 1700000000, plan = 'free', freeLines = 5, runs = {  } }\n");
    }

    #[test]
    fn a_malformed_cap_entry_is_dropped_alone_and_a_fractional_cap_rounds() {
        let json = r#"{"v":3,"generatedAt":"2026-09-22T10:00:00.000Z","plan":"pro","freeLines":5,"runs":[],"groups":["G"],"caps":[{"i":"nope"},{"i":7,"c":99.6,"l":0,"g":0,"m":false},7]}"#;
        let parsed: WireRuns = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.caps.len(), 1);
        assert_eq!(parsed.caps[0].cap, 100);
    }

    #[test]
    fn a_malformed_group_name_keeps_its_slot_so_later_indexes_do_not_shift() {
        use mlua::{Lua, LuaOptions, StdLib};

        let json = r#"{"v":3,"generatedAt":"2026-09-22T10:00:00.000Z","plan":"pro","freeLines":5,"runs":[],"groups":["Transmog",7,"Ore","  "],"caps":[{"i":1,"c":100,"g":2},{"i":2,"c":100,"g":1},{"i":3,"c":100,"g":3}]}"#;
        let parsed: WireRuns = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.groups.len(), 4, "{:?}", parsed.groups);
        let lua_source = render_runs_lua(&parsed, 1_700_000_000).unwrap();
        // The cap in "Ore" still says "Ore"; the caps in the unreadable group
        // and in the blank-named one say nothing.
        assert!(lua_source.contains("groups = { 'Transmog', '', 'Ore', '  ' }, caps = { { i = 1, c = 100, g = 3 }, { i = 2, c = 100 }, { i = 3, c = 100 } }"), "{lua_source}");

        let lua = Lua::new_with(StdLib::NONE, LuaOptions::default()).unwrap();
        lua.load(&lua_source).exec().unwrap();
        let label: String = lua
            .load("local r = GoldCap_AppRuns; return r.groups[r.caps[1].g]")
            .eval()
            .unwrap();
        assert_eq!(label, "Ore");
    }

    #[test]
    fn a_cap_without_a_group_on_the_wire_names_no_group() {
        let json = r#"{"v":3,"generatedAt":"2026-09-22T10:00:00.000Z","plan":"pro","freeLines":5,"runs":[],"groups":["Transmog"],"caps":[{"i":5,"c":100},{"i":6,"c":100,"g":null},{"i":7,"c":100,"g":"x"},{"i":8,"c":100,"g":0},{"i":9,"c":100,"g":-1}]}"#;
        let parsed: WireRuns = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.caps[4].group, None, "a negative index is no group");
        let lua = render_runs_lua(&parsed, 1_700_000_000).unwrap();
        assert!(lua.contains("caps = { { i = 5, c = 100 }, { i = 6, c = 100 }, { i = 7, c = 100 }, { i = 8, c = 100, g = 1 }, { i = 9, c = 100 } }"), "{lua}");
    }

    #[test]
    fn caps_render_after_runs_with_one_based_group_indexes_and_defaults_omitted() {
        let mut runs = v3_empty();
        runs.groups = vec!["Transmog".into(), "O'Ore".into()];
        runs.caps = vec![
            WireCap { item_id: 212345, cap: 1_500_000, min_ilvl: 610, group: Some(0), manual: true },
            WireCap { item_id: 190311, cap: 4200, min_ilvl: 0, group: Some(1), manual: false },
        ];
        let lua = render_runs_lua(&runs, 1_700_000_000).unwrap();
        assert!(lua.ends_with(", groups = { 'Transmog', 'O\\'Ore' }, caps = { { i = 212345, c = 1500000, l = 610, g = 1, m = true }, { i = 190311, c = 4200, g = 2 } } }\n"), "{lua}");
    }

    #[test]
    fn a_cap_the_addon_could_not_use_is_dropped_whole() {
        let mut runs = v3_empty();
        runs.groups = vec!["G".into()];
        runs.caps = vec![
            WireCap { item_id: 0, cap: 100, min_ilvl: 0, group: Some(0), manual: false },     // no item
            WireCap { item_id: 5, cap: 0, min_ilvl: 0, group: Some(0), manual: false },       // no price
            WireCap { item_id: 6, cap: 100, min_ilvl: 0, group: Some(9), manual: false },     // group index off the end → kept, without g
            WireCap { item_id: 7, cap: 100, min_ilvl: 0, group: Some(0), manual: false },
        ];
        let lua = render_runs_lua(&runs, 1_700_000_000).unwrap();
        assert!(lua.contains("caps = { { i = 6, c = 100 }, { i = 7, c = 100, g = 1 } }"), "{lua}");
    }

    #[test]
    fn only_unusable_caps_means_no_caps_key_at_all() {
        let mut runs = v3_empty();
        runs.groups = vec!["G".into()];
        runs.caps = vec![WireCap { item_id: 0, cap: 0, min_ilvl: 0, group: Some(0), manual: false }];
        let lua = render_runs_lua(&runs, 1_700_000_000).unwrap();
        assert!(!lua.contains("caps") && !lua.contains("groups"), "{lua}");
    }

    #[test]
    fn a_v3_render_with_caps_loads_in_a_sandboxed_lua_vm() {
        use mlua::{Lua, LuaOptions, StdLib};

        let mut runs = v3_empty();
        runs.groups = vec!["Transmog".into()];
        runs.caps = vec![
            WireCap { item_id: 212345, cap: 1_500_000, min_ilvl: 610, group: Some(0), manual: true },
            WireCap { item_id: 190311, cap: 4200, min_ilvl: 0, group: Some(0), manual: false },
        ];
        let lua_source = render_runs_lua(&runs, 1_700_000_000).unwrap();

        let lua = Lua::new_with(StdLib::NONE, LuaOptions::default()).unwrap();
        lua.load(&lua_source).exec().unwrap();

        let i: i64 = lua
            .load("return GoldCap_AppRuns.caps[1].i")
            .eval()
            .unwrap();
        assert_eq!(i, 212345);
        let c: i64 = lua
            .load("return GoldCap_AppRuns.caps[1].c")
            .eval()
            .unwrap();
        assert_eq!(c, 1_500_000);
        let l: i64 = lua
            .load("return GoldCap_AppRuns.caps[1].l")
            .eval()
            .unwrap();
        assert_eq!(l, 610);
        let g: i64 = lua
            .load("return GoldCap_AppRuns.caps[1].g")
            .eval()
            .unwrap();
        assert_eq!(g, 1);
        let m: bool = lua
            .load("return GoldCap_AppRuns.caps[1].m")
            .eval()
            .unwrap();
        assert!(m);
        let group_name: String = lua
            .load("return GoldCap_AppRuns.groups[1]")
            .eval()
            .unwrap();
        assert_eq!(group_name, "Transmog");
        let l2: Option<i64> = lua
            .load("return GoldCap_AppRuns.caps[2].l")
            .eval()
            .unwrap();
        assert_eq!(l2, None);
        let m2: Option<bool> = lua
            .load("return GoldCap_AppRuns.caps[2].m")
            .eval()
            .unwrap();
        assert_eq!(m2, None);
    }

    /// A v3 alert run with one gear line on it, for the item-level floor
    /// tests below. `floor` is spliced into the line as it stands: `""` for
    /// no key at all, `,"minIlvl":625` for a floor.
    fn gear_alert_json(floor: &str) -> String {
        format!(
            r#"{{"v":3,"generatedAt":"2026-09-23T10:00:00.000Z","plan":"pro","freeLines":5,"runs":[{{"code":"a0000016","name":"Gear watch","updatedAt":"2026-09-15T09:00:00.000Z","kind":"alert","sharedBy":null,"source":null,"lines":[{{"itemId":222440,"qty":1,"vendor":false,"nameEn":"Everforged Longsword","usual":null,"vendorUnit":null,"cheapHour":null,"cap":1500000,"realm":{{"id":1305,"name":"Kazzak"}},"craft":null{floor}}}]}}]}}"#
        )
    }

    /// What this build wrote for `gear_alert_json("")` before a line could
    /// carry a floor — the file a line without one must still produce.
    fn gear_alert_lua_without_a_floor() -> String {
        let updated_at = crate::ledger_summary::parse_iso_utc("2026-09-15T09:00:00.000Z").unwrap();
        format!(
            "GoldCap_AppRuns = {{ v = 3, generatedAt = 1789000000, plan = 'pro', freeLines = 5, runs = {{ {{ code = 'a0000016', name = 'Gear watch', k = 'alert', updatedAt = {updated_at}, lines = {{ {{ i = 222440, q = 1, v = false, n = 'Everforged Longsword', cc = 1500000, rl = {{ id = 1305, n = 'Kazzak' }} }} }} }} }} }}\n"
        )
    }

    #[test]
    fn a_gear_line_renders_its_min_ilvl_after_its_cap() {
        let parsed: WireRuns = serde_json::from_str(&gear_alert_json(r#","minIlvl":625"#)).unwrap();
        assert_eq!(parsed.runs[0].lines[0].min_ilvl, Some(625));
        let lua = render_runs_lua(&parsed, 1_789_000_000).unwrap();
        let updated_at = crate::ledger_summary::parse_iso_utc("2026-09-15T09:00:00.000Z").unwrap();
        let expected = format!(
            "GoldCap_AppRuns = {{ v = 3, generatedAt = 1789000000, plan = 'pro', freeLines = 5, runs = {{ {{ code = 'a0000016', name = 'Gear watch', k = 'alert', updatedAt = {updated_at}, lines = {{ {{ i = 222440, q = 1, v = false, n = 'Everforged Longsword', cc = 1500000, minIlvl = 625, rl = {{ id = 1305, n = 'Kazzak' }} }} }} }} }} }}\n"
        );
        assert_eq!(lua, expected);
    }

    #[test]
    fn a_line_without_a_floor_renders_byte_for_byte_what_this_build_always_wrote() {
        // Nearly every file has no gated gear on it, and none of those may
        // change by a byte: no key, a null and a zero all mean "any level".
        for floor in ["", r#","minIlvl":null"#, r#","minIlvl":0"#] {
            let parsed: WireRuns = serde_json::from_str(&gear_alert_json(floor)).unwrap();
            let lua = render_runs_lua(&parsed, 1_789_000_000).unwrap();
            assert_eq!(lua, gear_alert_lua_without_a_floor(), "floor {floor:?}");
        }
    }

    #[test]
    fn a_malformed_min_ilvl_drops_the_floor_and_keeps_the_line() {
        // A floor this build cannot read costs the floor and nothing else:
        // failing the payload would strand Runs.lua at yesterday's file, and
        // dropping the line would take a firing alert off the BUY tab.
        for floor in [
            r#""625""#,
            "-625",
            "625.5",
            "4294967296",
            "true",
            "[625]",
            r#"{"l":625}"#,
        ] {
            let json = gear_alert_json(&format!(r#","minIlvl":{floor}"#));
            let parsed: WireRuns = serde_json::from_str(&json)
                .unwrap_or_else(|e| panic!("minIlvl {floor} failed the payload: {e}"));
            assert_eq!(parsed.runs[0].lines[0].min_ilvl, None, "minIlvl {floor}");
            let lua = render_runs_lua(&parsed, 1_789_000_000).unwrap();
            assert_eq!(lua, gear_alert_lua_without_a_floor(), "minIlvl {floor}");
        }
    }

    #[test]
    fn a_min_ilvl_loads_in_a_sandboxed_lua_vm_and_a_line_without_one_reads_nil() {
        use mlua::{Lua, LuaOptions, StdLib};

        let json = r#"{"v":3,"generatedAt":"2026-09-23T10:00:00.000Z","plan":"pro","freeLines":5,"runs":[{"code":"a0000016","name":"Gear watch","updatedAt":"2026-09-15T09:00:00.000Z","kind":"alert","lines":[{"itemId":222440,"qty":1,"vendor":false,"nameEn":"Everforged Longsword","cap":1500000,"minIlvl":625},{"itemId":212264,"qty":3,"vendor":false,"nameEn":"Flask of Alchemical Chaos","cap":99900}]}]}"#;
        let parsed: WireRuns = serde_json::from_str(json).unwrap();
        let lua_source = render_runs_lua(&parsed, 1_789_000_000).unwrap();

        let lua = Lua::new_with(StdLib::NONE, LuaOptions::default()).unwrap();
        lua.load(&lua_source).exec().unwrap();

        let floor: Option<i64> = lua
            .load("return GoldCap_AppRuns.runs[1].lines[1].minIlvl")
            .eval()
            .unwrap();
        assert_eq!(floor, Some(625));
        let none: Option<i64> = lua
            .load("return GoldCap_AppRuns.runs[1].lines[2].minIlvl")
            .eval()
            .unwrap();
        assert_eq!(none, None);
    }
}
