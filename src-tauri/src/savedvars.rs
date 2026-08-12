//! Reads the addon's ledger out of `SavedVariables/GoldCap.lua`.
//!
//! WoW rewrites this file wholesale at logout, so the companion treats it as
//! strictly READ-ONLY: anything written here would be destroyed, and writing
//! while the client is running risks corrupting the player's saved state.
//! Which entries have already been uploaded is therefore tracked in the
//! companion's own state file, not in the addon's.

use mlua::{Lua, LuaOptions, StdLib, Table, Value};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct LedgerEntry {
    pub key: String,
    pub kind: String,
    pub source: String,
    #[serde(rename = "itemID", skip_serializing_if = "Option::is_none")]
    pub item_id: Option<i64>,
    #[serde(rename = "itemName", skip_serializing_if = "Option::is_none")]
    pub item_name: Option<String>,
    pub qty: i64,
    pub total: i64,
    pub cut: i64,
    pub deposit: i64,
    pub pending: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mv: Option<i64>,
    pub at: i64,
    #[serde(rename = "char", skip_serializing_if = "Option::is_none")]
    pub character: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    #[serde(rename = "decisionVersion", skip_serializing_if = "Option::is_none")]
    pub decision_version: Option<i64>,
    #[serde(rename = "decisionStatus", skip_serializing_if = "Option::is_none")]
    pub decision_status: Option<String>,
    #[serde(rename = "decisionReasons", skip_serializing_if = "Option::is_none")]
    pub decision_reasons: Option<Vec<String>>,
    #[serde(rename = "stressUnit", skip_serializing_if = "Option::is_none")]
    pub stress_unit: Option<i64>,
    #[serde(rename = "expectedProfit", skip_serializing_if = "Option::is_none")]
    pub expected_profit: Option<i64>,
    #[serde(rename = "recommendedQuantity", skip_serializing_if = "Option::is_none")]
    pub recommended_quantity: Option<i64>,
    #[serde(rename = "sourceAt", skip_serializing_if = "Option::is_none")]
    pub source_at: Option<i64>,
}

struct DecisionEvidence {
    decision_version: i64,
    decision_status: String,
    decision_reasons: Vec<String>,
    stress_unit: i64,
    expected_profit: i64,
    recommended_quantity: i64,
    source_at: i64,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct GoldPoint {
    #[serde(rename = "char")]
    pub character: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    pub copper: i64,
    pub at: i64,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct LedgerData {
    pub entries: Vec<LedgerEntry>,
    pub gold: Vec<GoldPoint>,
}

/// Every `WTF/Account/<ACCOUNT>/SavedVariables/GoldCap.lua` on disk. One per
/// Battle.net account on this machine — a player with two accounts has two
/// separate ledgers and both are theirs.
pub fn saved_variables_paths(wow_retail_path: &Path) -> Vec<PathBuf> {
    let account_root = wow_retail_path.join("WTF").join("Account");
    let Ok(accounts) = std::fs::read_dir(&account_root) else {
        return Vec::new();
    };
    accounts
        .flatten()
        .map(|a| a.path().join("SavedVariables").join("GoldCap.lua"))
        .filter(|p| p.is_file())
        .collect()
}

fn opt_string(t: &Table, key: &str) -> Option<String> {
    match t.get::<Value>(key) {
        Ok(Value::String(s)) => s.to_str().ok().map(|s| s.to_string()),
        _ => None,
    }
}

fn opt_int(t: &Table, key: &str) -> Option<i64> {
    match t.get::<Value>(key) {
        Ok(Value::Integer(i)) => Some(i),
        Ok(Value::Number(n)) => Some(n as i64),
        _ => None,
    }
}

fn opt_exact_int(t: &Table, key: &str) -> Option<i64> {
    match t.get::<Value>(key) {
        Ok(Value::Integer(i)) => Some(i),
        Ok(Value::Number(n))
            if n.is_finite()
                && n.fract() == 0.0
                && n > i64::MIN as f64
                && n < i64::MAX as f64 =>
        {
            Some(n as i64)
        }
        _ => None,
    }
}

fn flag(t: &Table, key: &str) -> bool {
    matches!(t.get::<Value>(key), Ok(Value::Boolean(true)))
}

fn opt_string_sequence(t: &Table, key: &str) -> Option<Vec<String>> {
    let Ok(Value::Table(values)) = t.get::<Value>(key) else {
        return None;
    };

    values
        .sequence_values::<Value>()
        .map(|value| match value {
            Ok(Value::String(value)) => value.to_str().ok().map(|value| value.to_string()),
            _ => None,
        })
        .collect()
}

// The upload API accepts decision evidence only as a complete seven-field
// group. A legacy row has no evidence, but a partial group would reject the
// whole batch, so any absent or malformed member makes this group absent.
fn decision_evidence(t: &Table) -> Option<DecisionEvidence> {
    const MAX_COPPER: i64 = 1_000_000_000_000_000;
    let decision_version = opt_exact_int(t, "decisionVersion")?;
    let decision_status = opt_string(t, "decisionStatus")?;
    let decision_reasons = opt_string_sequence(t, "decisionReasons")?;
    let stress_unit = opt_exact_int(t, "stressUnit")?;
    let expected_profit = opt_exact_int(t, "expectedProfit")?;
    let recommended_quantity = opt_exact_int(t, "recommendedQuantity")?;
    let source_at = opt_exact_int(t, "sourceAt")?;

    if !(1..=32_767).contains(&decision_version)
        || !matches!(decision_status.as_str(), "SAFE" | "WATCH" | "AVOID")
        || decision_reasons.len() > 32
        || decision_reasons
            .iter()
            .any(|reason| reason.is_empty() || reason.encode_utf16().count() > 64)
        || !(0..=MAX_COPPER).contains(&stress_unit)
        || !(0..=MAX_COPPER).contains(&expected_profit)
        || recommended_quantity <= 0
        || source_at <= 0
    {
        return None;
    }

    Some(DecisionEvidence {
        decision_version,
        decision_status,
        decision_reasons,
        stress_unit,
        expected_profit,
        recommended_quantity,
        source_at,
    })
}

/// Evaluates the file in a VM with NO standard library, so the "code" in it can
/// only build tables — it cannot open a file, spawn a process or reach the
/// network even if a hostile file tried.
pub fn parse_saved_variables(lua_source: &str) -> Result<LedgerData, String> {
    let lua = Lua::new_with(StdLib::NONE, LuaOptions::default()).map_err(|e| e.to_string())?;
    lua.load(lua_source).exec().map_err(|e| e.to_string())?;

    let Ok(Value::Table(db)) = lua.globals().get::<Value>("GoldCapDB") else {
        return Ok(LedgerData::default());
    };

    let mut data = LedgerData::default();

    if let Ok(Value::Table(ledger)) = db.get::<Value>("ledger") {
        for row in ledger.sequence_values::<Table>().flatten() {
            // key/kind/source/qty/total/at are the fields an upload cannot do
            // without: no key means no dedupe, and a row that can't be deduped
            // would land again on every single sync.
            let (Some(key), Some(kind), Some(source), Some(qty), Some(total), Some(at)) = (
                opt_string(&row, "key"),
                opt_string(&row, "kind"),
                opt_string(&row, "source"),
                opt_int(&row, "qty"),
                opt_int(&row, "total"),
                opt_int(&row, "at"),
            ) else {
                continue;
            };
            let evidence = decision_evidence(&row);
            data.entries.push(LedgerEntry {
                key,
                kind,
                source,
                item_id: opt_int(&row, "itemID"),
                item_name: opt_string(&row, "itemName"),
                qty,
                total,
                cut: opt_int(&row, "cut").unwrap_or(0),
                deposit: opt_int(&row, "deposit").unwrap_or(0),
                pending: flag(&row, "pending"),
                mv: opt_int(&row, "mv"),
                at,
                character: opt_string(&row, "char"),
                region: opt_string(&row, "region"),
                decision_version: evidence.as_ref().map(|e| e.decision_version),
                decision_status: evidence.as_ref().map(|e| e.decision_status.clone()),
                decision_reasons: evidence.as_ref().map(|e| e.decision_reasons.clone()),
                stress_unit: evidence.as_ref().map(|e| e.stress_unit),
                expected_profit: evidence.as_ref().map(|e| e.expected_profit),
                recommended_quantity: evidence.as_ref().map(|e| e.recommended_quantity),
                source_at: evidence.as_ref().map(|e| e.source_at),
            });
        }
    }

    if let Ok(Value::Table(gold)) = db.get::<Value>("gold") {
        for row in gold.sequence_values::<Table>().flatten() {
            let (Some(character), Some(copper), Some(at)) = (
                opt_string(&row, "char"),
                opt_int(&row, "copper"),
                opt_int(&row, "at"),
            ) else {
                continue;
            };
            data.gold.push(GoldPoint {
                character,
                region: opt_string(&row, "region"),
                copper,
                at,
            });
        }
    }

    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real SavedVariables file written by the live game, with the character
    /// name and balances swapped out. Structure — key order, nesting, the
    /// control characters inside `key`, the fields the addon does and doesn't
    /// write — is exactly what WoW produced.
    const REAL_FILE: &str = include_str!("../tests/fixtures/GoldCap.lua");

    #[test]
    fn the_fixture_still_has_the_windows_line_endings_the_game_writes() {
        // WoW writes SavedVariables with CRLF. An earlier version of this
        // fixture was copied through a text-mode read that silently converted
        // them, so the parser was being tested against bytes the game never
        // produces. Guarded here because a .gitattributes rule, an editor or
        // a well-meaning formatter could quietly undo it again.
        assert!(REAL_FILE.contains("\r\n"), "fixture lost its CRLF line endings");
    }

    #[test]
    fn reads_a_real_savedvariables_file() {
        let data = parse_saved_variables(REAL_FILE).unwrap();
        assert_eq!(data.entries.len(), 2);
        assert_eq!(data.gold.len(), 5);
    }

    #[test]
    fn a_real_sale_has_a_name_and_no_item_id() {
        let data = parse_saved_variables(REAL_FILE).unwrap();
        let sale = data.entries.iter().find(|e| e.kind == "sale").unwrap();

        assert_eq!(sale.source, "mail");
        assert_eq!(sale.item_name.as_deref(), Some("Carving Canine"));
        // The sale mail carries money, not the item, so there is no id to read
        // — the server resolves the name instead.
        assert_eq!(sale.item_id, None);
        assert_eq!(sale.qty, 14);
        assert_eq!(sale.total, 200_200);
        assert_eq!(sale.cut, 10_010); // exactly the auction house's 5%
        assert_eq!(sale.deposit, 5_250);
        assert!(!sale.pending);
        assert_eq!(sale.character.as_deref(), Some("Testchar-Dentarg"));
        assert_eq!(sale.region.as_deref(), Some("eu"));
    }

    #[test]
    fn a_real_buy_does_carry_its_item_id() {
        let data = parse_saved_variables(REAL_FILE).unwrap();
        let buy = data.entries.iter().find(|e| e.kind == "buy").unwrap();

        assert_eq!(buy.item_id, Some(7676)); // Thistle Tea
        assert_eq!(buy.item_name.as_deref(), Some("Thistle Tea"));
        assert_eq!(buy.qty, 1);
        assert_eq!(buy.total, 27_500);
        assert_eq!(buy.cut, 0);
    }

    #[test]
    fn the_dedupe_key_survives_its_control_characters() {
        // EntryKey joins fields with \1. It has to reach the server byte for
        // byte or idempotency breaks and every sale lands twice.
        let data = parse_saved_variables(REAL_FILE).unwrap();
        let sale = data.entries.iter().find(|e| e.kind == "sale").unwrap();
        assert!(sale.key.contains('\u{1}'), "separator lost: {:?}", sale.key);
        assert_eq!(sale.key.split('\u{1}').count(), 8);
        assert!(sale.key.starts_with("Auction House\u{1}Carving Canine\u{1}"));
    }

    #[test]
    fn a_real_entry_survives_a_json_round_trip_under_the_addons_field_names() {
        // The wire format is the addon's own vocabulary, because the API's
        // zod schema was written against it.
        let data = parse_saved_variables(REAL_FILE).unwrap();
        let buy = data.entries.iter().find(|e| e.kind == "buy").unwrap();
        let json = serde_json::to_value(buy).unwrap();

        assert_eq!(json["itemID"], 7676);
        assert_eq!(json["itemName"], "Thistle Tea");
        assert_eq!(json["char"], "Testchar-Dentarg");
        assert_eq!(json["key"], buy.key.as_str());
        // Absent optional fields must not serialise as nulls — the API treats
        // an explicit null differently from a missing key.
        assert!(json.get("mv").is_none());
    }

    #[test]
    fn a_sniper_decision_preserves_the_exact_total_and_optional_evidence_on_the_wire() {
        let src = r#"
GoldCapDB = { ["ledger"] = {
    {
        ["key"] = "sniper-evidence", ["kind"] = "buy", ["source"] = "goldcap_sniper",
        ["itemID"] = 210930, ["qty"] = 2, ["total"] = 4321, ["at"] = 1785600000,
        ["decisionVersion"] = 2, ["decisionStatus"] = "WATCH",
        ["decisionReasons"] = { "roi_thin", "supply_tight" },
        ["stressUnit"] = 12500, ["expectedProfit"] = 85000,
        ["recommendedQuantity"] = 3, ["sourceAt"] = 1785600100,
    },
} }
"#;

        let data = parse_saved_variables(src).unwrap();
        assert_eq!(data.entries.len(), 1);
        let entry = &data.entries[0];
        assert_eq!(entry.total, 4321, "the quoted purchase total must remain exact");
        let json = serde_json::to_value(entry).unwrap();

        assert_eq!(json["decisionVersion"], 2);
        assert_eq!(json["decisionStatus"], "WATCH");
        assert_eq!(json["decisionReasons"], serde_json::json!(["roi_thin", "supply_tight"]));
        assert_eq!(json["stressUnit"], 12500);
        assert_eq!(json["expectedProfit"], 85000);
        assert_eq!(json["recommendedQuantity"], 3);
        assert_eq!(json["sourceAt"], 1785600100);
    }

    #[test]
    fn a_malformed_decision_reason_element_clears_the_whole_evidence_group() {
        let src = r#"
GoldCapDB = { ["ledger"] = {
    {
        ["key"] = "bad-reason", ["kind"] = "buy", ["source"] = "goldcap_sniper",
        ["qty"] = 1, ["total"] = 99, ["at"] = 1785600000,
        ["decisionVersion"] = 1, ["decisionStatus"] = "SAFE",
        ["decisionReasons"] = { "roi_above_floor", 42 },
        ["stressUnit"] = 50, ["expectedProfit"] = 75,
        ["recommendedQuantity"] = 1, ["sourceAt"] = 1785600000,
    },
} }
"#;

        let data = parse_saved_variables(src).unwrap();
        assert_eq!(data.entries.len(), 1, "bad optional evidence must not discard the ledger row");
        let json = serde_json::to_value(&data.entries[0]).unwrap();
        assert_eq!(json["key"], "bad-reason");
        for key in [
            "decisionVersion",
            "decisionStatus",
            "decisionReasons",
            "stressUnit",
            "expectedProfit",
            "recommendedQuantity",
            "sourceAt",
        ] {
            assert!(json.get(key).is_none(), "malformed evidence leaked {key}");
        }
    }

    #[test]
    fn a_missing_decision_scalar_clears_the_whole_evidence_group() {
        let src = r#"
GoldCapDB = { ["ledger"] = {
    {
        ["key"] = "missing-scalar", ["kind"] = "buy", ["source"] = "goldcap_sniper",
        ["qty"] = 1, ["total"] = 99, ["at"] = 1785600000,
        ["decisionVersion"] = 1, ["decisionStatus"] = "SAFE",
        ["decisionReasons"] = { "roi_above_floor" },
        ["stressUnit"] = 50, ["recommendedQuantity"] = 1, ["sourceAt"] = 1785600000,
    },
} }
"#;

        let data = parse_saved_variables(src).unwrap();
        assert_eq!(data.entries.len(), 1, "missing optional evidence must not discard the ledger row");
        let json = serde_json::to_value(&data.entries[0]).unwrap();
        assert_eq!(json["key"], "missing-scalar");
        for key in [
            "decisionVersion",
            "decisionStatus",
            "decisionReasons",
            "stressUnit",
            "expectedProfit",
            "recommendedQuantity",
            "sourceAt",
        ] {
            assert!(json.get(key).is_none(), "incomplete evidence leaked {key}");
        }
    }

    #[test]
    fn a_malformed_decision_scalar_clears_the_whole_evidence_group() {
        let src = r#"
GoldCapDB = { ["ledger"] = {
    {
        ["key"] = "malformed-scalar", ["kind"] = "buy", ["source"] = "goldcap_sniper",
        ["qty"] = 1, ["total"] = 99, ["at"] = 1785600000,
        ["decisionVersion"] = 1, ["decisionStatus"] = "SAFE",
        ["decisionReasons"] = { "roi_above_floor" },
        ["stressUnit"] = 50, ["expectedProfit"] = "not-a-number",
        ["recommendedQuantity"] = 1, ["sourceAt"] = 1785600000,
    },
} }
"#;

        let data = parse_saved_variables(src).unwrap();
        assert_eq!(data.entries.len(), 1, "malformed optional evidence must not discard the ledger row");
        let json = serde_json::to_value(&data.entries[0]).unwrap();
        assert_eq!(json["key"], "malformed-scalar");
        for key in [
            "decisionVersion",
            "decisionStatus",
            "decisionReasons",
            "stressUnit",
            "expectedProfit",
            "recommendedQuantity",
            "sourceAt",
        ] {
            assert!(json.get(key).is_none(), "malformed evidence leaked {key}");
        }
    }

    #[test]
    fn semantically_invalid_decision_fields_clear_the_whole_evidence_group() {
        for (key, status, expected_profit) in [
            ("invalid-status", r#""UNSURE""#, "75"),
            ("fractional-profit", r#""SAFE""#, "75.5"),
        ] {
            let src = format!(
                r#"
GoldCapDB = {{ ["ledger"] = {{
    {{
        ["key"] = "{key}", ["kind"] = "buy", ["source"] = "goldcap_sniper",
        ["qty"] = 1, ["total"] = 99, ["at"] = 1785600000,
        ["decisionVersion"] = 1, ["decisionStatus"] = {status},
        ["decisionReasons"] = {{ "roi_above_floor" }},
        ["stressUnit"] = 50, ["expectedProfit"] = {expected_profit},
        ["recommendedQuantity"] = 1, ["sourceAt"] = 1785600000,
    }},
}} }}
"#
            );

            let data = parse_saved_variables(&src).unwrap();
            assert_eq!(data.entries.len(), 1, "invalid evidence must not discard the ledger row");
            let json = serde_json::to_value(&data.entries[0]).unwrap();
            for evidence_key in [
                "decisionVersion",
                "decisionStatus",
                "decisionReasons",
                "stressUnit",
                "expectedProfit",
                "recommendedQuantity",
                "sourceAt",
            ] {
                assert!(json.get(evidence_key).is_none(), "invalid evidence leaked {evidence_key}");
            }
        }
    }

    #[test]
    fn a_legacy_entry_serializes_without_decision_evidence_keys() {
        let src = r#"
GoldCapDB = { ["ledger"] = {
    { ["key"] = "legacy", ["kind"] = "sale", ["source"] = "mail", ["qty"] = 1, ["total"] = 17, ["at"] = 1785600000 },
} }
"#;

        let data = parse_saved_variables(src).unwrap();
        assert_eq!(data.entries.len(), 1);
        assert_eq!(
            serde_json::to_value(&data.entries[0]).unwrap(),
            serde_json::json!({
                "key": "legacy",
                "kind": "sale",
                "source": "mail",
                "qty": 1,
                "total": 17,
                "cut": 0,
                "deposit": 0,
                "pending": false,
                "at": 1785600000,
            }),
        );
    }

    #[test]
    fn reads_the_gold_series_in_file_order() {
        let data = parse_saved_variables(REAL_FILE).unwrap();
        assert_eq!(data.gold[0].copper, 1_000_000_000);
        assert_eq!(data.gold[0].character, "Testchar-Dentarg");
        assert_eq!(data.gold[0].region.as_deref(), Some("eu"));
        assert!(data.gold.windows(2).all(|w| w[0].at <= w[1].at));
    }

    #[test]
    fn a_file_without_a_ledger_yields_nothing_rather_than_failing() {
        // A player who has the addon but has never sold anything is the normal
        // first-run case, not an error to report in the tray.
        let data = parse_saved_variables("GoldCapDB = { [\"settings\"] = {} }").unwrap();
        assert!(data.entries.is_empty());
        assert!(data.gold.is_empty());
    }

    #[test]
    fn a_missing_global_yields_nothing() {
        let data = parse_saved_variables("SomethingElse = {}").unwrap();
        assert!(data.entries.is_empty());
    }

    #[test]
    fn malformed_lua_is_an_error_not_a_panic() {
        assert!(parse_saved_variables("GoldCapDB = {{{").is_err());
    }

    #[test]
    fn entries_missing_required_fields_are_skipped() {
        let src = r#"
GoldCapDB = { ["ledger"] = {
	{ ["kind"] = "sale" },
	{ ["key"] = "k", ["kind"] = "sale", ["source"] = "mail", ["qty"] = 1, ["total"] = 1, ["at"] = 1785600000 },
} }
"#;
        let data = parse_saved_variables(src).unwrap();
        assert_eq!(
            data.entries.len(),
            1,
            "a keyless row cannot be deduped, so it cannot be uploaded"
        );
    }

    #[test]
    fn the_file_cannot_reach_the_host() {
        // The VM is built with no stdlib: a SavedVariables file that somehow
        // contained code could not open a file or a socket even if it tried.
        assert!(parse_saved_variables("GoldCapDB = { x = os.time() }").is_err());
    }
}
