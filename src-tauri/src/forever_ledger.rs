//! WoW: Forever ledger. The addon writes the same `GoldCapDB.ledger` rows and `GoldCapDB.gold`
//! readings on Forever as on retail; this module sends every one of them from a Forever file to
//! `POST /v1/forever/ledger/upload`, never to the retail ledger route (the site refuses a Forever
//! passport there, and the two games' item ids overlap). The BUY-tab purchases still go to their
//! own route (`forever_runs`); this pass does not change that one.
//!
//! "Already sent" works like the retail upload: a per-file set of row fingerprints
//! (`upload::entry_fingerprint` / `gold_fingerprint`), so a row re-written under the same key with
//! a new total, pending flag or region goes up again.

use crate::forever_runs::{PurchaseOutcome, PurchasePass};
use crate::savedvars::{ClientPassport, GoldPoint, LedgerEntry};
use serde::Serialize;
use std::collections::BTreeSet;
use std::path::Path;

/// Must not exceed the site's own caps for entries and for gold readings in one request.
pub const MAX_BATCH: usize = 500;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PassportBody<'a> {
    interface: i64,
    build: &'a str,
    region_id: i64,
}

/// One ledger row on the wire: the retail row's names, without the sniper decision fields.
#[derive(Serialize)]
struct WireEntry<'a> {
    key: &'a str,
    kind: &'a str,
    source: &'a str,
    #[serde(rename = "itemID", skip_serializing_if = "Option::is_none")]
    item_id: Option<i64>,
    #[serde(rename = "itemName", skip_serializing_if = "Option::is_none")]
    item_name: Option<&'a str>,
    qty: i64,
    total: i64,
    cut: i64,
    deposit: i64,
    pending: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    mv: Option<i64>,
    at: i64,
    #[serde(rename = "char", skip_serializing_if = "Option::is_none")]
    character: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    region: Option<&'a str>,
    #[serde(rename = "runCode", skip_serializing_if = "Option::is_none")]
    run_code: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    faction: Option<&'a str>,
}

#[derive(Serialize)]
struct WireGold<'a> {
    #[serde(rename = "char")]
    character: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    region: Option<&'a str>,
    copper: i64,
    at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    faction: Option<&'a str>,
}

#[derive(Serialize)]
struct UploadBody<'a> {
    client: PassportBody<'a>,
    entries: Vec<WireEntry<'a>>,
    gold: Vec<WireGold<'a>>,
}

fn wire_entry(e: &LedgerEntry) -> WireEntry<'_> {
    WireEntry {
        key: &e.key,
        kind: &e.kind,
        source: &e.source,
        item_id: e.item_id,
        item_name: e.item_name.as_deref(),
        qty: e.qty,
        total: e.total,
        cut: e.cut,
        deposit: e.deposit,
        pending: e.pending,
        mv: e.mv,
        at: e.at,
        character: e.character.as_deref(),
        region: e.region.as_deref(),
        run_code: e.run_code.as_deref(),
        faction: e.faction.as_deref(),
    }
}

fn wire_gold(g: &GoldPoint) -> WireGold<'_> {
    WireGold { character: &g.character, region: g.region.as_deref(), copper: g.copper, at: g.at, faction: g.faction.as_deref() }
}

/// One request to the Forever ledger route under the pairing token and the file's own passport.
pub async fn upload_ledger(
    client: &reqwest::Client,
    base: &str,
    token: &str,
    passport: &ClientPassport,
    region_id: i64,
    entries: &[LedgerEntry],
    gold: &[GoldPoint],
) -> PurchaseOutcome {
    let body = UploadBody {
        client: PassportBody { interface: passport.interface, build: &passport.build, region_id },
        entries: entries.iter().map(wire_entry).collect(),
        gold: gold.iter().map(wire_gold).collect(),
    };
    let sent = client
        .post(format!("{base}/v1/forever/ledger/upload"))
        .bearer_auth(token)
        .header(crate::upload::CLIENT_HEADER, passport.header_value())
        .json(&body)
        .send()
        .await;
    match sent {
        Err(e) => PurchaseOutcome::Retry(crate::sync::describe_err(&e, crate::sync::CLIENT_TIMEOUT)),
        Ok(res) => match res.status().as_u16() {
            // 400/409/413/422 (409 includes the site's beta refusal): this batch will never be
            // taken as it is, so resending it every tick helps nobody. Marked done like a 200.
            200 | 400 | 409 | 413 | 422 => PurchaseOutcome::Done,
            401 => PurchaseOutcome::Unauthorized,
            code => PurchaseOutcome::Retry(format!("status {code}")),
        },
    }
}

/// One file's pass: read it, forget fingerprints it no longer holds, send what is new. Ledger rows
/// go in requests of their own, then gold readings in requests of their own (each request carries
/// the other array empty).
pub async fn send_ledger(
    client: &reqwest::Client,
    base: &str,
    token: &str,
    file: &Path,
    sent: &mut BTreeSet<String>,
    logger: &crate::logging::Logger,
) -> PurchasePass {
    let Ok(source) = std::fs::read_to_string(file) else { return PurchasePass::Incomplete };
    let data = match crate::savedvars::parse_saved_variables(&source) {
        Ok(d) => d,
        Err(e) => {
            logger.error(&format!("{}: not readable yet ({e})", file.display()));
            return PurchasePass::Incomplete;
        }
    };
    // Only a file that says it is WoW: Forever's, and says which region it is: the site needs both.
    let Some((passport, region_id)) = data
        .client
        .as_ref()
        .filter(|p| crate::forever::is_forever_interface(p.interface))
        .and_then(|p| Some((p, p.region_id?)))
    else {
        return PurchasePass::Complete;
    };
    let entry_prints: Vec<String> = data.entries.iter().map(crate::upload::entry_fingerprint).collect();
    let gold_prints: Vec<String> = data.gold.iter().map(crate::upload::gold_fingerprint).collect();
    let present: BTreeSet<&str> = entry_prints.iter().chain(&gold_prints).map(String::as_str).collect();
    sent.retain(|p| present.contains(p.as_str()));

    let pending_entries: Vec<(&LedgerEntry, &String)> =
        data.entries.iter().zip(&entry_prints).filter(|(_, p)| !sent.contains(*p)).collect();
    let pending_gold: Vec<(&GoldPoint, &String)> =
        data.gold.iter().zip(&gold_prints).filter(|(_, p)| !sent.contains(*p)).collect();
    let (mut entries_sent, mut gold_sent) = (0usize, 0usize);

    for batch in pending_entries.chunks(MAX_BATCH) {
        let rows: Vec<LedgerEntry> = batch.iter().map(|(e, _)| (*e).clone()).collect();
        match upload_ledger(client, base, token, passport, region_id, &rows, &[]).await {
            PurchaseOutcome::Done => {
                sent.extend(batch.iter().map(|(_, p)| (*p).clone()));
                entries_sent += batch.len();
            }
            PurchaseOutcome::Unauthorized => return PurchasePass::Unauthorized,
            PurchaseOutcome::Retry(why) => {
                logger.error(&format!("forever ledger from {}: will retry ({why})", file.display()));
                return PurchasePass::Incomplete;
            }
        }
    }
    for batch in pending_gold.chunks(MAX_BATCH) {
        let rows: Vec<GoldPoint> = batch.iter().map(|(g, _)| (*g).clone()).collect();
        match upload_ledger(client, base, token, passport, region_id, &[], &rows).await {
            PurchaseOutcome::Done => {
                sent.extend(batch.iter().map(|(_, p)| (*p).clone()));
                gold_sent += batch.len();
            }
            PurchaseOutcome::Unauthorized => return PurchasePass::Unauthorized,
            PurchaseOutcome::Retry(why) => {
                logger.error(&format!("forever ledger from {}: will retry ({why})", file.display()));
                return PurchasePass::Incomplete;
            }
        }
    }
    if entries_sent + gold_sent > 0 {
        logger.info(&format!("forever ledger from {}: sent ({entries_sent} rows, {gold_sent} gold readings)", file.display()));
    }
    PurchasePass::Complete
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forever::tests::{answer, serve_forever, serve_once};

    const FILE: &str = r#"GoldCapDB = { client = { interface = 16001, build = "1.60.1.70790", regionId = 3 }, ledger = {
      { key = "sale-1", kind = "sale", source = "mail", itemID = 2589, itemName = "Linen Cloth", qty = 31, total = 744, cut = 37, deposit = 100, pending = false, mv = 70, at = 1793979118, char = "Meolys-CAMWOW EU5 Realm 7 PvE", region = "eu", faction = "Horde" },
      { key = "buy-1", kind = "buy", source = "goldcap_sniper", itemID = 2589, qty = 1, total = 60, at = 1793979200, decisionVersion = 1 },
      { key = "post-1", kind = "post", source = "auction", qty = 5, total = 400, pending = true, at = 1793979300, faction = "Neutral" },
    }, gold = {
      { char = "Meolys-CAMWOW EU5 Realm 7 PvE", region = "eu", copper = 3080, at = 1793900679, faction = "Horde" },
      { char = "Alt-CAMWOW EU5 Realm 7 PvE", copper = 99, at = 1793900700 },
    } }"#;

    fn write(dir: &std::path::Path, lua: &str) -> std::path::PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let file = dir.join("GoldCap.lua");
        std::fs::write(&file, lua).unwrap();
        file
    }

    fn body_of(request: &str) -> serde_json::Value {
        serde_json::from_str(request.split("\r\n\r\n").nth(1).unwrap()).unwrap()
    }

    #[tokio::test]
    async fn every_kind_source_and_gold_reading_goes_up_with_the_passport() {
        let dir = std::env::temp_dir().join(format!("goldcap-forever-ledger-all-{}", std::process::id()));
        let file = write(&dir, FILE);
        let logger = crate::logging::Logger::new(&dir.join("logs")).unwrap();
        let mut sent = BTreeSet::new();
        let (base, seen) = serve_forever(vec![answer("200 OK", "{}"), answer("200 OK", "{}")]);
        let pass = send_ledger(&crate::sync::build_client(), &base, "tok", &file, &mut sent, &logger).await;
        assert!(matches!(pass, PurchasePass::Complete));
        let requests: Vec<String> = seen.try_iter().collect();
        assert_eq!(requests.len(), 2, "rows in one request, gold in another");
        let lower = requests[0].to_ascii_lowercase();
        assert!(lower.starts_with("post /v1/forever/ledger/upload "), "{}", requests[0]);
        assert!(lower.contains("x-goldcap-client: 16001/1.60.1.70790"));
        assert!(lower.contains("authorization: bearer tok"));

        let rows = body_of(&requests[0]);
        assert_eq!(rows["client"], serde_json::json!({ "interface": 16001, "build": "1.60.1.70790", "regionId": 3 }));
        assert_eq!(
            rows["entries"][0],
            serde_json::json!({ "key": "sale-1", "kind": "sale", "source": "mail", "itemID": 2589, "itemName": "Linen Cloth", "qty": 31, "total": 744,
              "cut": 37, "deposit": 100, "pending": false, "mv": 70, "at": 1793979118, "char": "Meolys-CAMWOW EU5 Realm 7 PvE", "region": "eu", "faction": "Horde" })
        );
        assert_eq!(rows["entries"].as_array().unwrap().len(), 3);
        assert!(rows["entries"][1].get("faction").is_none() && rows["entries"][1].get("decisionVersion").is_none());
        assert!(rows["entries"][2].get("faction").is_none(), "a faction other than the two is dropped");
        assert_eq!(rows["entries"][2]["source"], "auction");
        assert_eq!(rows["gold"], serde_json::json!([]));

        let gold = body_of(&requests[1]);
        assert_eq!(gold["entries"], serde_json::json!([]));
        assert_eq!(
            gold["gold"],
            serde_json::json!([
                { "char": "Meolys-CAMWOW EU5 Realm 7 PvE", "region": "eu", "copper": 3080, "at": 1793900679, "faction": "Horde" },
                { "char": "Alt-CAMWOW EU5 Realm 7 PvE", "copper": 99, "at": 1793900700 }
            ])
        );
        assert_eq!(sent.len(), 5);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn run_code_rides_with_a_buy_tab_row() {
        let dir = std::env::temp_dir().join(format!("goldcap-forever-ledger-run-{}", std::process::id()));
        let lua = r#"GoldCapDB = { client = { interface = 16001, build = "1.60.1.70790", regionId = 3 }, ledger = {
          { key = "b", kind = "buy", source = "goldcap_buy", itemID = 2589, qty = 53, total = 3551, runCode = "k7f3qzab", at = 1790000000 } } }"#;
        let file = write(&dir, lua);
        let logger = crate::logging::Logger::new(&dir.join("logs")).unwrap();
        let (base, seen) = serve_once(answer("200 OK", "{}"));
        send_ledger(&crate::sync::build_client(), &base, "tok", &file, &mut BTreeSet::new(), &logger).await;
        assert_eq!(body_of(&seen.recv().unwrap())["entries"][0]["runCode"], "k7f3qzab");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn a_retail_file_and_a_file_without_a_region_send_nothing() {
        let dir = std::env::temp_dir().join(format!("goldcap-forever-ledger-skip-{}", std::process::id()));
        let logger = crate::logging::Logger::new(&dir.join("logs")).unwrap();
        let retail = write(&dir, &FILE.replace("interface = 16001, build = \"1.60.1.70790\"", "interface = 120100, build = \"12.1.0.69933\""));
        let mut sent = BTreeSet::new();
        let pass = send_ledger(&crate::sync::build_client(), "http://127.0.0.1:9", "tok", &retail, &mut sent, &logger).await;
        assert!(matches!(pass, PurchasePass::Complete));
        assert!(sent.is_empty());
        let no_region = write(&dir, &FILE.replace(", regionId = 3", ""));
        let pass = send_ledger(&crate::sync::build_client(), "http://127.0.0.1:9", "tok", &no_region, &mut sent, &logger).await;
        assert!(matches!(pass, PurchasePass::Complete));
        assert!(sent.is_empty());
        let unstamped = write(&dir, &FILE.replace("client = { interface = 16001, build = \"1.60.1.70790\", regionId = 3 }, ", ""));
        let pass = send_ledger(&crate::sync::build_client(), "http://127.0.0.1:9", "tok", &unstamped, &mut sent, &logger).await;
        assert!(matches!(pass, PurchasePass::Complete));
        assert!(sent.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn fingerprints_are_kept_after_200_and_409_and_forgotten_when_gone() {
        let dir = std::env::temp_dir().join(format!("goldcap-forever-ledger-prints-{}", std::process::id()));
        let file = write(&dir, FILE);
        let logger = crate::logging::Logger::new(&dir.join("logs")).unwrap();
        let mut sent: BTreeSet<String> = ["gone|print".to_string()].into();

        let (base, _seen) = serve_once(answer("500 Internal Server Error", "{}"));
        let pass = send_ledger(&crate::sync::build_client(), &base, "tok", &file, &mut sent, &logger).await;
        assert!(matches!(pass, PurchasePass::Incomplete));
        assert!(sent.is_empty(), "a print no longer in the file is forgotten; nothing was taken");

        // Rows taken (200), gold refused as a beta client (409): both are done.
        let (base, _seen) = serve_forever(vec![answer("200 OK", "{}"), answer("409 Conflict", r#"{"error":"beta"}"#)]);
        let pass = send_ledger(&crate::sync::build_client(), &base, "tok", &file, &mut sent, &logger).await;
        assert!(matches!(pass, PurchasePass::Complete));
        assert_eq!(sent.len(), 5);

        // Nothing pending: no request at all (nothing listens on port 9).
        let pass = send_ledger(&crate::sync::build_client(), "http://127.0.0.1:9", "tok", &file, &mut sent, &logger).await;
        assert!(matches!(pass, PurchasePass::Complete));

        // A row re-written under the same key with a new total and a settled pending flag goes again.
        let changed = FILE.replace("total = 744", "total = 800").replace("pending = true", "pending = false");
        write(&dir, &changed);
        let (base, seen) = serve_once(answer("200 OK", "{}"));
        let pass = send_ledger(&crate::sync::build_client(), &base, "tok", &file, &mut sent, &logger).await;
        assert!(matches!(pass, PurchasePass::Complete));
        let rows = body_of(&seen.recv().unwrap());
        let keys: Vec<&str> = rows["entries"].as_array().unwrap().iter().map(|e| e["key"].as_str().unwrap()).collect();
        assert_eq!(keys, vec!["sale-1", "post-1"]);
        assert_eq!(sent.len(), 5);

        // A row gone from the file leaves the set.
        write(&dir, &changed.replace(r#"{ key = "buy-1", kind = "buy", source = "goldcap_sniper", itemID = 2589, qty = 1, total = 60, at = 1793979200, decisionVersion = 1 },"#, ""));
        send_ledger(&crate::sync::build_client(), "http://127.0.0.1:9", "tok", &file, &mut sent, &logger).await;
        assert_eq!(sent.len(), 4);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn a_refused_token_stops_the_pass() {
        let dir = std::env::temp_dir().join(format!("goldcap-forever-ledger-401-{}", std::process::id()));
        let file = write(&dir, FILE);
        let logger = crate::logging::Logger::new(&dir.join("logs")).unwrap();
        let mut sent = BTreeSet::new();
        let (base, _seen) = serve_once(answer("401 Unauthorized", r#"{"error":"unauthorized"}"#));
        let pass = send_ledger(&crate::sync::build_client(), &base, "tok", &file, &mut sent, &logger).await;
        assert!(matches!(pass, PurchasePass::Unauthorized));
        assert!(sent.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_parser_keeps_only_alliance_and_horde() {
        let lua = r#"GoldCapDB = { client = { interface = 16001, build = "1.60.1.70790", regionId = 3 }, ledger = {
          { key = "a", kind = "sale", source = "mail", qty = 1, total = 1, at = 1, faction = "Alliance" },
          { key = "b", kind = "sale", source = "mail", qty = 1, total = 1, at = 1, faction = "Horde" },
          { key = "c", kind = "sale", source = "mail", qty = 1, total = 1, at = 1, faction = "Neutral" },
          { key = "d", kind = "sale", source = "mail", qty = 1, total = 1, at = 1 },
        }, gold = { { char = "X", copper = 1, at = 1, faction = "Alliance" }, { char = "Y", copper = 1, at = 1, faction = "horde" } } }"#;
        let data = crate::savedvars::parse_saved_variables(lua).unwrap();
        let f: Vec<Option<&str>> = data.entries.iter().map(|e| e.faction.as_deref()).collect();
        assert_eq!(f, vec![Some("Alliance"), Some("Horde"), None, None]);
        assert_eq!(data.gold[0].faction.as_deref(), Some("Alliance"));
        assert_eq!(data.gold[1].faction, None);
        assert_eq!(data.client.unwrap().region_id, Some(3));
    }
}
