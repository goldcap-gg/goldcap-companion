//! Wire shape, `Runs.lua` renderer, fetch, and write seam for the "Buy runs"
//! feature — same shape as `ledger_summary.rs`, rides the sync tick as a
//! passenger after it (see `sync.rs::sync_once`).

use crate::ledger_summary::parse_iso_utc;
use crate::luafile::escape_lua_string;
use serde::Deserialize;
use std::path::Path;

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
/// re-derived from its ISO timestamp the same way `LedgerSummary.lua`'s
/// `sale_rows` does; an unparseable timestamp fails the whole render rather
/// than silently rendering as the Unix epoch — the caller (Task 2) must not
/// write a file that lies about a run's freshness.
pub fn render_runs_lua(runs: &WireRuns, generated_at: i64) -> Result<String, String> {
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
        let updated_at = parse_iso_utc(&run.updated_at).ok_or_else(|| {
            format!("runs: bad updatedAt {:?} on run {}", run.updated_at, run.code)
        })?;
        out.push_str(&format!("updatedAt = {}, lines = {{ ", updated_at));
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
        return Err(format!("runs: http {}", res.status()));
    }
    res.json::<WireRuns>().await.map_err(|e| e.to_string())
}

/// The write seam `sync_once` calls: `Ok(true)` means it wrote, and every
/// failure — a fetch error, an unsupported `v`, or an unparseable
/// `updatedAt` inside `render_runs_lua` — comes back as `Err` before any
/// filesystem operation, leaving a previously written `Runs.lua` untouched.
/// `Ok(false)` is part of the shared shape with `ledger_summary`'s seam but
/// is never actually returned here.
pub fn apply_fetch_result(
    dir: &Path,
    fetched: Result<WireRuns, String>,
    generated_at: i64,
) -> Result<bool, String> {
    let runs = fetched?;
    if runs.v != 1 {
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
                lines: vec![WireRunLine {
                    item_id: 3,
                    qty: 1,
                    vendor: true,
                    name_en: "Deckhand's Shirt".into(),
                }],
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
                lines: vec![WireRunLine {
                    item_id: 3,
                    qty: 1,
                    vendor: true,
                    name_en: "Deckhand's Shirt".into(),
                }],
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
        assert_eq!(apply_fetch_result(dir.path(), Ok(runs), 7).unwrap(), true);
        let s = std::fs::read_to_string(dir.path().join(crate::luafile::RUNS_FILE_NAME)).unwrap();
        assert!(s.starts_with("GoldCap_AppRuns = { v = 1, generatedAt = 7"));
        assert!(!dir.path().join("Runs.lua.tmp").exists());
    }

    #[test]
    fn a_wrong_version_is_refused_before_writing() {
        let dir = tempfile::tempdir().unwrap();
        let runs = WireRuns {
            v: 2,
            generated_at: String::new(),
            plan: "free".into(),
            free_lines: 5,
            runs: vec![],
        };
        assert!(apply_fetch_result(dir.path(), Ok(runs), 7).is_err());
        assert!(!dir.path().join(crate::luafile::RUNS_FILE_NAME).exists());
    }
}
