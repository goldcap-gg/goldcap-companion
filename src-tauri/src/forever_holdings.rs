//! WoW: Forever holdings: the same `GoldCapDB.holdings` rows as retail (`holdings`), from a
//! Forever file to `POST /v1/forever/ledger/holdings` with the file's passport, never to the
//! retail route. The site files each row under its holder's realm, so no region is needed.
//!
//! Answers as the Forever owned-lots route: 200, a 400 other than `timestamp_in_future`, 409 and
//! 413 are the site's say and are marked sent; 401 is a refused token; 404 (an older site), 429,
//! 5xx and network errors mark nothing and the file is read again next tick.

use crate::forever_ledger::PassportBody;
use crate::forever_owned_lots::{outcome, LotsOutcome};
use crate::forever_runs::PurchasePass;
use crate::holdings::{pending, stamp, Batch, HoldingsSent};
use serde::Serialize;
use std::path::Path;

pub const ROUTE: &str = "/v1/forever/ledger/holdings";

#[derive(Serialize)]
struct UploadBody<'a> {
    client: PassportBody<'a>,
    #[serde(flatten)]
    batch: &'a Batch<'a>,
}

/// One file's pass: read it, forget places it no longer holds, send what is new in batches.
pub async fn send_holdings(
    client: &reqwest::Client,
    base: &str,
    token: &str,
    file: &Path,
    sent: &mut HoldingsSent,
    logger: &crate::logging::Logger,
) -> PurchasePass {
    let Ok(source) = std::fs::read_to_string(file) else { return PurchasePass::Incomplete };
    let holdings = match crate::savedvars::parse_holdings(&source) {
        Ok(Some(h)) => h,
        Ok(None) => return PurchasePass::Complete,
        Err(e) => {
            logger.error(&format!("{}: not readable yet ({e})", file.display()));
            return PurchasePass::Incomplete;
        }
    };
    // Only a file that says it is WoW: Forever's, and says which region it is: the site needs both.
    let Some((passport, region_id)) = holdings
        .client
        .as_ref()
        .filter(|p| crate::forever::is_forever_interface(p.interface))
        .and_then(|p| Some((p, p.region_id?)))
    else {
        return PurchasePass::Complete;
    };

    let mut taken = 0usize;
    for batch in pending(&holdings, sent) {
        let body = UploadBody { client: PassportBody { interface: passport.interface, build: &passport.build, region_id }, batch: &batch };
        let answer = client
            .post(format!("{base}{ROUTE}"))
            .bearer_auth(token)
            .header(crate::upload::CLIENT_HEADER, passport.header_value())
            .json(&body)
            .send()
            .await;
        match outcome(answer).await {
            LotsOutcome::Taken => {
                stamp(sent, &batch);
                taken += batch.rows();
            }
            LotsOutcome::Early => {
                logger.info(&format!("forever holdings from {}: dated ahead of the site's clock, sent again later", file.display()));
                return PurchasePass::Incomplete;
            }
            LotsOutcome::Unauthorized => return PurchasePass::Unauthorized,
            LotsOutcome::Retry(why) => {
                logger.error(&format!("forever holdings from {}: will retry ({why})", file.display()));
                return PurchasePass::Incomplete;
            }
        }
    }
    if taken > 0 {
        logger.info(&format!("forever holdings from {}: sent ({taken} rows)", file.display()));
    }
    PurchasePass::Complete
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forever::tests::{answer, serve_once};

    const FILE: &str = include_str!("../tests/fixtures/GoldCap-holdings.lua");

    fn setup(name: &str) -> (std::path::PathBuf, std::path::PathBuf, crate::logging::Logger) {
        let dir = std::env::temp_dir().join(format!("goldcap-forever-holdings-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let lua = FILE.replacen("[\"interface\"] = 120100,", "[\"interface\"] = 16001, [\"regionId\"] = 3,", 1).replacen("12.1.0.69933", "1.60.1.70938", 1);
        std::fs::write(dir.join("GoldCap.lua"), lua).unwrap();
        let logger = crate::logging::Logger::new(&dir.join("logs")).unwrap();
        (dir.clone(), dir.join("GoldCap.lua"), logger)
    }

    #[tokio::test]
    async fn forever_holdings_go_up_with_the_passport_and_the_answer_decides_the_stamps() {
        let (dir, file, logger) = setup("answers");
        let client = crate::sync::build_client();

        let mut sent = HoldingsSent::default();
        let (base, seen) = serve_once(answer("200 OK", r#"{"accepted":10,"skipped":0,"sends":1}"#));
        assert!(matches!(send_holdings(&client, &base, "tok", &file, &mut sent, &logger).await, PurchasePass::Complete));
        let request = seen.recv().unwrap();
        assert!(request.to_ascii_lowercase().contains("x-goldcap-client: 16001/1.60.1.70938"));
        let body: serde_json::Value = serde_json::from_str(request.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(body["client"], serde_json::json!({ "interface": 16001, "build": "1.60.1.70938", "regionId": 3 }));
        assert_eq!(body["readings"].as_array().unwrap().len(), 10);
        assert_eq!(sent.readings.len(), 10);
        assert_eq!(sent.sends_at, 1794597000);

        for status in ["409 Conflict", "400 Bad Request", "413 Payload Too Large"] {
            let mut sent = HoldingsSent::default();
            let (base, _seen) = serve_once(answer(status, r#"{"error":"beta"}"#));
            assert!(matches!(send_holdings(&client, &base, "tok", &file, &mut sent, &logger).await, PurchasePass::Complete), "{status}");
            assert_eq!(sent.readings.len(), 10, "{status}");
        }
        for (status, body) in [
            ("400 Bad Request", r#"{"error":"timestamp_in_future"}"#),
            ("429 Too Many Requests", "{}"),
            ("500 Internal Server Error", "{}"),
        ] {
            let mut sent = HoldingsSent::default();
            let (base, _seen) = serve_once(answer(status, body));
            assert!(matches!(send_holdings(&client, &base, "tok", &file, &mut sent, &logger).await, PurchasePass::Incomplete), "{status}");
            assert_eq!(sent, HoldingsSent::default(), "{status}");
        }
        let mut sent = HoldingsSent::default();
        let (base, _seen) = serve_once(answer("401 Unauthorized", "{}"));
        assert!(matches!(send_holdings(&client, &base, "tok", &file, &mut sent, &logger).await, PurchasePass::Unauthorized));
        assert_eq!(sent, HoldingsSent::default());
        std::fs::remove_dir_all(&dir).ok();
    }
}
