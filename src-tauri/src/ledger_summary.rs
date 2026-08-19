//! Fetches the companion's own ledger summary from goldcap.gg and writes the
//! subset the in-game Sold tab renders into the `GoldCap_AppData` mini-addon
//! as `LedgerSummary.lua`. Rides the sync tick as a passenger after the
//! upload leg — see the sold-tab design doc
//! (docs/superpowers/specs/2026-08-19-sold-tab-companion-ledger-design.md).

use serde::Deserialize;

/// The consumed subset of `GET /v1/ledger/summary/companion`. Unknown wire
/// fields are ignored by serde's default, so the endpoint may grow freely.
/// `lastUploadedAt` is deliberately not consumed — the file's `generatedAt`
/// (stamped at fetch time, Task 2) is the only age surface the tab needs.
#[derive(Debug, Deserialize)]
pub struct WireSummary {
    pub pro: bool,
    pub days: u32,
    pub totals: WireTotals,
    pub recent: Vec<WireRecent>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireTotals {
    pub proceeds_copper: i64,
    pub spent_copper: i64,
    pub pending_proceeds_copper: i64,
    pub sales_count: i64,
    /// Pro-only; absent on the free tier. Absence must survive into the Lua
    /// file (omitted key), never become a fake 0.
    pub realized_copper: Option<i64>,
    pub median_hold_seconds: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireRecent {
    pub kind: String,
    pub item_id: Option<i64>,
    pub item_name: Option<String>,
    pub quantity: i64,
    pub total_copper: i64,
    pub cut_copper: i64,
    pub pending: bool,
    pub occurred_at: String,
    pub basis: Option<WireBasis>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireBasis {
    pub matched_quantity: i64,
    pub unmatched_quantity: i64,
    pub cost_copper: i64,
    pub profit_copper: i64,
}

/// One renderable sale row: `recent` filtered to sales, timestamps already
/// unix. A row whose timestamp fails to parse is dropped, not guessed.
#[derive(Debug)]
pub struct SaleRow {
    pub name: String,
    pub item_id: Option<i64>,
    pub qty: i64,
    pub total: i64,
    pub cut: i64,
    pub pending: bool,
    pub at: i64,
    pub basis: Option<WireBasis>,
}

pub fn sale_rows(summary: &WireSummary) -> Vec<SaleRow> {
    summary
        .recent
        .iter()
        .filter(|r| r.kind == "sale")
        .filter_map(|r| {
            let at = parse_iso_utc(&r.occurred_at)?;
            Some(SaleRow {
                name: r.item_name.clone().unwrap_or_else(|| "Unknown item".to_string()),
                item_id: r.item_id,
                qty: r.quantity,
                total: r.total_copper,
                cut: r.cut_copper,
                pending: r.pending,
                at,
                basis: r.basis.clone(),
            })
        })
        .collect()
}

/// Parses the strict UTC ISO-8601 shape the API emits
/// (`YYYY-MM-DDTHH:MM:SS[.fff]Z`) to unix seconds. Hand-rolled because the
/// dependency tree has no datetime crate and this one fixed shape does not
/// justify adding one to a signed release app. Fractional seconds are
/// truncated. Offset forms are rejected — the API only emits Zulu.
pub fn parse_iso_utc(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 20 || !s.ends_with('Z') {
        return None;
    }
    if b[4] != b'-' || b[7] != b'-' || b[10] != b'T' || b[13] != b':' || b[16] != b':' {
        return None;
    }
    let num = |r: std::ops::Range<usize>| -> Option<i64> { s.get(r)?.parse().ok() };
    let (y, mo, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (h, mi, se) = (num(11..13)?, num(14..16)?, num(17..19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || se > 59 {
        return None;
    }
    // Days-from-civil (Howard Hinnant's algorithm), exact for all Gregorian dates.
    let y2 = if mo <= 2 { y - 1 } else { y };
    let era = (if y2 >= 0 { y2 } else { y2 - 399 }) / 400;
    let yoe = y2 - era * 400;
    let doy = (153 * (if mo > 2 { mo - 3 } else { mo + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + h * 3600 + mi * 60 + se)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_iso_utc_handles_epoch_and_real_timestamps() {
        assert_eq!(parse_iso_utc("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_iso_utc("1970-01-02T00:00:00.000Z"), Some(86_400));
        // Cross-checked against a real AppData writtenAt observed live:
        // 2026-08-19 09:04:22 UTC == 1787130262.
        assert_eq!(parse_iso_utc("2026-08-19T09:04:22.000Z"), Some(1_787_130_262));
    }

    #[test]
    fn parse_iso_utc_rejects_garbage_and_non_utc() {
        assert_eq!(parse_iso_utc(""), None);
        assert_eq!(parse_iso_utc("not-a-date"), None);
        assert_eq!(parse_iso_utc("2026-08-19T09:04:22"), None); // no Z
        assert_eq!(parse_iso_utc("2026-08-19T09:04:22+03:00"), None); // offset form
        assert_eq!(parse_iso_utc("2026-99-19T09:04:22Z"), None); // month out of range
    }

    #[test]
    fn wire_summary_deserializes_and_ignores_unknown_fields() {
        // Trimmed real response shape, plus fields we deliberately do NOT
        // consume (lastUploadedAt, coverage, daily, topItems, openLots) to
        // prove the endpoint may grow or carry more without breaking us.
        let json = r#"{
          "pro": true, "days": 30, "lastUploadedAt": "2026-08-19T09:00:00.000Z",
          "coverage": {"explainedFraction": 0.9},
          "totals": {"proceedsCopper": 100, "spentCopper": 50,
                     "pendingProceedsCopper": 7, "salesCount": 2,
                     "realizedCopper": 25, "medianHoldSeconds": 3600,
                     "openCapitalCopper": 1},
          "daily": [], "topItems": [], "openLots": [],
          "recent": [
            {"id": 2, "kind": "sale", "source": "mail", "region": "eu",
             "itemId": 190396, "itemName": "Umbral Tin Ore", "quantity": 2,
             "totalCopper": 100, "cutCopper": 5, "pending": false,
             "occurredAt": "1970-01-02T00:00:00.000Z",
             "basis": {"matchedQuantity": 2, "unmatchedQuantity": 0,
                       "costCopper": 50, "profitCopper": 45}},
            {"id": 1, "kind": "buy", "source": "goldcap_sniper", "region": "eu",
             "itemId": 5, "itemName": "X", "quantity": 1, "totalCopper": 10,
             "cutCopper": 0, "pending": false,
             "occurredAt": "1970-01-01T00:00:00.000Z"}
          ]
        }"#;
        let s: WireSummary = serde_json::from_str(json).unwrap();
        assert!(s.pro);
        assert_eq!(s.days, 30);
        assert_eq!(s.totals.realized_copper, Some(25));
        assert_eq!(s.recent.len(), 2);
        assert_eq!(s.recent[0].basis.as_ref().unwrap().profit_copper, 45);
    }

    #[test]
    fn free_tier_totals_deserialize_without_pro_fields() {
        let json = r#"{"proceedsCopper": 1, "spentCopper": 2,
                       "pendingProceedsCopper": 3, "salesCount": 4}"#;
        let t: WireTotals = serde_json::from_str(json).unwrap();
        assert_eq!(t.realized_copper, None);
        assert_eq!(t.median_hold_seconds, None);
    }

    #[test]
    fn sale_rows_keeps_sales_only_and_drops_unparseable_timestamps() {
        let summary = WireSummary {
            pro: false,
            days: 30,
            totals: WireTotals {
                proceeds_copper: 0, spent_copper: 0, pending_proceeds_copper: 0,
                sales_count: 0, realized_copper: None, median_hold_seconds: None,
            },
            recent: vec![
                WireRecent { kind: "sale".into(), item_id: None, item_name: None,
                    quantity: 1, total_copper: 10, cut_copper: 0, pending: true,
                    occurred_at: "1970-01-01T00:00:10Z".into(), basis: None },
                WireRecent { kind: "buy".into(), item_id: Some(5), item_name: Some("X".into()),
                    quantity: 1, total_copper: 10, cut_copper: 0, pending: false,
                    occurred_at: "1970-01-01T00:00:00Z".into(), basis: None },
                WireRecent { kind: "sale".into(), item_id: Some(6), item_name: Some("Y".into()),
                    quantity: 1, total_copper: 10, cut_copper: 0, pending: false,
                    occurred_at: "when?".into(), basis: None }, // dropped, never guessed
            ],
        };
        let rows = sale_rows(&summary);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].at, 10);
        assert_eq!(rows[0].name, "Unknown item");
        assert!(rows[0].pending);
    }
}
