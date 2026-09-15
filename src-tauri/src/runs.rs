//! Wire shape, `Runs.lua` renderer, fetch, and write seam for the "Buy runs"
//! feature — same shape as `ledger_summary.rs`, rides the sync tick as a
//! passenger after it (see `sync.rs::sync_once`). Contract v1 carried a line's
//! id, quantity, vendor flag and English name; v2 adds the site's reference
//! price, the vendor's unit price and the region's cheap hour, all optional,
//! so a v1 server and a v2 server both render through the same code.

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

fn lenient_opt_i64<'de, D>(d: D) -> Result<Option<i64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<LenientNumber>::deserialize(d)?.and_then(|n| n.to_i64()))
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
    #[serde(default, deserialize_with = "lenient_opt_i64")]
    pub usual: Option<i64>,
    #[serde(default, deserialize_with = "lenient_opt_i64")]
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
    #[serde(default, deserialize_with = "lenient_opt_i64")]
    pub usual: Option<i64>,
    /// What a vendor charges per unit, in copper. Absent when nobody sells it.
    #[serde(default, deserialize_with = "lenient_opt_i64")]
    pub vendor_unit: Option<i64>,
    #[serde(default)]
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
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WireRun {
    pub code: String,
    #[serde(default)]
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

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WireRuns {
    pub v: u32,
    pub generated_at: String,
    pub plan: String,
    pub free_lines: u32,
    pub runs: Vec<WireRun>,
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
            out.push_str(" }");
        }
        out.push_str(" } }");
    }
    out.push_str(" } }\n");
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
/// the optional per-line price facts. An unknown version is refused rather
/// than rendered half-understood — the addon would then be reading a file
/// whose meaning this build cannot vouch for, and a stale correct file beats
/// a fresh misread one.
const SUPPORTED_VERSIONS: [u32; 2] = [1, 2];

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

    #[test]
    fn renders_the_addon_table_shape() {
        let runs = WireRuns {
            v: 1,
            generated_at: "2026-09-15T10:00:00.000Z".into(),
            plan: "pro".into(),
            free_lines: 5,
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
            v: 3,
            generated_at: String::new(),
            plan: "free".into(),
            free_lines: 5,
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
        for key in [", u = ", ", vu = ", ", ch = ", ", cp = "] {
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
}
