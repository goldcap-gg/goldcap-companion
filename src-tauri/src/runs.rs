//! Wire shape for the "Buy runs" feature and the `Runs.lua` renderer. Task 2
//! adds the fetch; this module only owns the deserialized shape and the pure
//! render function, same split as `ledger_summary.rs`.

use crate::ledger_summary::parse_iso_utc;
use crate::luafile::escape_lua_string;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WireRunLine {
    pub item_id: u32,
    pub qty: u32,
    pub vendor: bool,
    pub name_en: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WireRun {
    pub code: String,
    pub name: Option<String>,
    pub updated_at: String,
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
/// on a line) to keep the in-game table small. `updatedAt` on each run is
/// re-derived from its ISO timestamp the same way `LedgerSummary.lua` does;
/// an unparseable timestamp renders as `0` rather than failing the whole
/// file — one bad run must not blank the others.
pub fn render_runs_lua(runs: &WireRuns, generated_at: i64) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "GoldCap_AppRuns = {{ v = 1, generatedAt = {}, plan = '{}', freeLines = {}, runs = {{ ",
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
        out.push_str(&format!(
            "updatedAt = {}, lines = {{ ",
            parse_iso_utc(&run.updated_at).unwrap_or(0)
        ));
        for (li, line) in run.lines.iter().enumerate() {
            if li > 0 {
                out.push_str(", ");
            }
            out.push_str(&format!(
                "{{ i = {}, q = {}, v = {}, n = '{}' }}",
                line.item_id,
                line.qty,
                line.vendor,
                escape_lua_string(&line.name_en)
            ));
        }
        out.push_str(" } }");
    }
    out.push_str(" } }\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

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
                lines: vec![WireRunLine {
                    item_id: 5,
                    qty: 210,
                    vendor: false,
                    name_en: "Plant Protein".into(),
                }],
            }],
        };
        let lua = render_runs_lua(&runs, 1_789_000_000);
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
                lines: vec![WireRunLine {
                    item_id: 3,
                    qty: 1,
                    vendor: true,
                    name_en: "Deckhand's Shirt".into(),
                }],
            }],
        };
        let lua = render_runs_lua(&runs, 1);
        assert!(!lua.contains("name ="));
        assert!(lua.contains("n = 'Deckhand\\'s Shirt'"));
        assert!(lua.contains("v = true"));
    }

    #[test]
    fn the_wire_shape_deserialises_from_the_api_json() {
        let json = r#"{"v":1,"generatedAt":"2026-09-15T10:00:00.000Z","plan":"free","freeLines":5,"runs":[{"code":"abcd2345","name":null,"updatedAt":"2026-09-15T09:00:00.000Z","lines":[{"itemId":5,"qty":210,"vendor":false,"nameEn":"Plant Protein"}]}]}"#;
        let parsed: WireRuns = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.runs[0].lines[0].qty, 210);
    }
}
