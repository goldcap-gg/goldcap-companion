//! WoW: Forever BUY purchases. The addon writes one `goldcap_buy` ledger row per BUY-tab purchase
//! into the Forever install's `GoldCapDB.ledger`, exactly as it does on retail; this module reads
//! those rows (with the retail parser — the row shape is the same) and sends them to
//! `POST /v1/forever/run-purchases`, never to the retail ledger route (the site refuses a Forever
//! passport there with 409, and the two games' item ids overlap). Contract: docs/companion/AGENTS.md
//! in the site repository, "Forever BUY lists and purchases".

use crate::savedvars::{ClientPassport, LedgerData};
use serde::Serialize;
use std::collections::BTreeSet;
use std::path::Path;

/// Must not exceed the site's own cap (FOREVER_RUN_PURCHASES_MAX).
pub const MAX_BATCH: usize = 200;

/// One purchase on the wire, under the site's field names.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunPurchase {
    pub key: String,
    pub run_code: String,
    pub item_id: i64,
    pub qty: i64,
    pub total: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mv: Option<i64>,
    pub at: i64,
    #[serde(rename = "char", skip_serializing_if = "Option::is_none")]
    pub character: Option<String>,
}

#[derive(Serialize)]
struct PassportBody<'a> {
    interface: i64,
    build: &'a str,
}

#[derive(Serialize)]
struct UploadBody<'a> {
    client: PassportBody<'a>,
    purchases: &'a [RunPurchase],
}

/// The BUY-tab purchases in one file: a `goldcap_buy` buy with a well-formed run code (the parser
/// already drops a malformed one) and an item id. Anything else — a sniper buy, a sale, a BUY row
/// from before run codes existed — is not a run purchase.
pub fn run_purchases(data: &LedgerData) -> Vec<RunPurchase> {
    data.entries
        .iter()
        .filter(|e| e.kind == "buy" && e.source == "goldcap_buy")
        .filter_map(|e| {
            let run_code = e.run_code.clone()?;
            let item_id = e.item_id.filter(|id| *id > 0)?;
            (e.qty > 0 && e.total >= 0).then(|| RunPurchase {
                key: e.key.clone(),
                run_code,
                item_id,
                qty: e.qty,
                total: e.total,
                mv: e.mv.filter(|v| *v > 0),
                at: e.at,
                character: e.character.clone(),
            })
        })
        .collect()
}

pub enum PurchaseOutcome {
    Done,
    Unauthorized,
    Retry(String),
}

/// One batch to the Forever route under the pairing token and the file's own Forever passport.
pub async fn upload_run_purchases(
    client: &reqwest::Client,
    base: &str,
    token: &str,
    passport: &ClientPassport,
    batch: &[RunPurchase],
) -> PurchaseOutcome {
    let body = UploadBody { client: PassportBody { interface: passport.interface, build: &passport.build }, purchases: batch };
    let sent = client
        .post(format!("{base}/v1/forever/run-purchases"))
        .bearer_auth(token)
        .header(crate::upload::CLIENT_HEADER, passport.header_value())
        .json(&body)
        .send()
        .await;
    match sent {
        Err(e) => PurchaseOutcome::Retry(crate::sync::describe_err(&e, crate::sync::CLIENT_TIMEOUT)),
        Ok(res) => match res.status().as_u16() {
            // 400/409/413/422: this batch will never be taken as it is — resending it every tick
            // helps nobody. Marked done like an accepted one.
            200 | 400 | 409 | 413 | 422 => PurchaseOutcome::Done,
            401 => PurchaseOutcome::Unauthorized,
            code => PurchaseOutcome::Retry(format!("status {code}")),
        },
    }
}

pub enum PurchasePass {
    /// Everything in the file the site should have, it has (or the file is not Forever's).
    Complete,
    /// Something is still to send; try again next tick.
    Incomplete,
    /// The token was refused.
    Unauthorized,
}

/// One file's pass: read it, forget sent keys it no longer holds, send what is new in batches.
pub async fn send_run_purchases(
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
    // Only a file that says it is WoW: Forever's. An unstamped file found here is unknown, not
    // Forever — and never retail's either (docs/companion/AGENTS.md, "Client passport").
    let Some(passport) = data.client.as_ref().filter(|p| crate::forever::is_forever_interface(p.interface)) else {
        return PurchasePass::Complete;
    };
    let rows = run_purchases(&data);
    let present: BTreeSet<&str> = rows.iter().map(|r| r.key.as_str()).collect();
    sent.retain(|k| present.contains(k.as_str()));
    let pending: Vec<RunPurchase> = rows.into_iter().filter(|r| !sent.contains(&r.key)).collect();
    for batch in pending.chunks(MAX_BATCH) {
        match upload_run_purchases(client, base, token, passport, batch).await {
            PurchaseOutcome::Done => sent.extend(batch.iter().map(|r| r.key.clone())),
            PurchaseOutcome::Unauthorized => return PurchasePass::Unauthorized,
            PurchaseOutcome::Retry(why) => {
                logger.error(&format!("forever purchases from {}: will retry ({why})", file.display()));
                return PurchasePass::Incomplete;
            }
        }
    }
    PurchasePass::Complete
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forever::tests::{answer, serve_forever, serve_once};

    const FOREVER_BUYS: &str = r#"GoldCapDB = { client = { interface = 16001, build = "1.60.1.70009", regionId = 90 }, ledger = {
      { key = "buyrun-a", kind = "buy", source = "goldcap_buy", itemID = 2589, itemName = "Linen Cloth", qty = 53, total = 3551, runCode = "k7f3qzab", mv = 70, at = 1790000000, char = "Zugg-Classic Beta PvE 2", region = "us" },
      { key = "buyrun-b", kind = "buy", source = "goldcap_buy", itemID = 2589, qty = 67, total = 4556, runCode = "k7f3qzab", at = 1790000060 },
      { key = "sniper-1", kind = "buy", source = "goldcap_sniper", itemID = 2589, qty = 1, total = 60, at = 1790000100 },
      { key = "buyrun-c", kind = "buy", source = "goldcap_buy", itemID = 2589, qty = 1, total = 60, at = 1790000200 },
      { key = "sale-1", kind = "sale", source = "mail", qty = 5, total = 400, at = 1790000300 },
    } }"#;

    fn write(dir: &std::path::Path, lua: &str) -> std::path::PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let file = dir.join("GoldCap.lua");
        std::fs::write(&file, lua).unwrap();
        file
    }

    #[test]
    fn only_buy_tab_purchases_with_a_run_and_an_item_go_up() {
        let data = crate::savedvars::parse_saved_variables(FOREVER_BUYS).unwrap();
        let rows = run_purchases(&data);
        assert_eq!(rows.iter().map(|r| r.key.as_str()).collect::<Vec<_>>(), vec!["buyrun-a", "buyrun-b"]);
        assert_eq!(rows[0].mv, Some(70));
        assert_eq!(rows[1].mv, None);
        let json = serde_json::to_value(&rows[0]).unwrap();
        assert_eq!(json, serde_json::json!({ "key": "buyrun-a", "runCode": "k7f3qzab", "itemId": 2589, "qty": 53, "total": 3551, "mv": 70, "at": 1790000000, "char": "Zugg-Classic Beta PvE 2" }));
    }

    #[tokio::test]
    async fn the_upload_names_the_forever_passport_and_goes_to_the_forever_route() {
        let data = crate::savedvars::parse_saved_variables(FOREVER_BUYS).unwrap();
        let (base, seen) = serve_once(answer("200 OK", r#"{"accepted":2,"stored":2}"#));
        let outcome = upload_run_purchases(&crate::sync::build_client(), &base, "tok", data.client.as_ref().unwrap(), &run_purchases(&data)).await;
        assert!(matches!(outcome, PurchaseOutcome::Done));
        let request = seen.recv().unwrap();
        let lower = request.to_ascii_lowercase();
        assert!(lower.starts_with("post /v1/forever/run-purchases "), "{request}");
        assert!(lower.contains("x-goldcap-client: 16001/1.60.1.70009"));
        assert!(request.contains(r#""client":{"interface":16001,"build":"1.60.1.70009"}"#));
        assert!(!request.contains("sniper-1") && !request.contains("sale-1"));
    }

    #[tokio::test]
    async fn keys_are_marked_sent_only_when_the_site_took_them_and_forgotten_when_gone() {
        let dir = std::env::temp_dir().join(format!("goldcap-forever-buys-{}", std::process::id()));
        let file = write(&dir, FOREVER_BUYS);
        let logger = crate::logging::Logger::new(&dir.join("logs")).unwrap();
        let mut sent: std::collections::BTreeSet<String> = ["gone-key".to_string()].into();

        let (base, _seen) = serve_once(answer("500 Internal Server Error", "{}"));
        let pass = send_run_purchases(&crate::sync::build_client(), &base, "tok", &file, &mut sent, &logger).await;
        assert!(matches!(pass, PurchasePass::Incomplete));
        assert!(sent.is_empty(), "a key no longer in the file is forgotten; nothing was taken");

        let (base, _seen) = serve_forever(vec![answer("200 OK", r#"{"accepted":2,"stored":2}"#)]);
        let pass = send_run_purchases(&crate::sync::build_client(), &base, "tok", &file, &mut sent, &logger).await;
        assert!(matches!(pass, PurchasePass::Complete));
        assert_eq!(sent.iter().cloned().collect::<Vec<_>>(), vec!["buyrun-a".to_string(), "buyrun-b".to_string()]);

        // Nothing pending: no request at all (nothing listens on port 9).
        let pass = send_run_purchases(&crate::sync::build_client(), "http://127.0.0.1:9", "tok", &file, &mut sent, &logger).await;
        assert!(matches!(pass, PurchasePass::Complete));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn a_retail_file_sends_no_forever_purchases() {
        let dir = std::env::temp_dir().join(format!("goldcap-forever-buys-retail-{}", std::process::id()));
        let file = write(&dir, &FOREVER_BUYS.replace("interface = 16001, build = \"1.60.1.70009\"", "interface = 120100, build = \"12.1.0.69933\""));
        let logger = crate::logging::Logger::new(&dir.join("logs")).unwrap();
        let mut sent = std::collections::BTreeSet::new();
        let pass = send_run_purchases(&crate::sync::build_client(), "http://127.0.0.1:9", "tok", &file, &mut sent, &logger).await;
        assert!(matches!(pass, PurchasePass::Complete));
        assert!(sent.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn a_refused_token_stops_the_pass() {
        let dir = std::env::temp_dir().join(format!("goldcap-forever-buys-401-{}", std::process::id()));
        let file = write(&dir, FOREVER_BUYS);
        let logger = crate::logging::Logger::new(&dir.join("logs")).unwrap();
        let mut sent = std::collections::BTreeSet::new();
        let (base, _seen) = serve_once(answer("401 Unauthorized", r#"{"error":"unauthorized"}"#));
        let pass = send_run_purchases(&crate::sync::build_client(), &base, "tok", &file, &mut sent, &logger).await;
        assert!(matches!(pass, PurchasePass::Unauthorized));
        std::fs::remove_dir_all(&dir).ok();
    }
}
