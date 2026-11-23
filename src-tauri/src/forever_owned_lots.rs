//! WoW: Forever's own auctions. The addon writes the same `GoldCapDB.ownedLots` rows on Forever as
//! on retail; this module sends them from a Forever file to `POST /v1/forever/owned-lots`, never to
//! the retail route (`/v1/owned-lots`, which refuses a Forever passport with 409, and the two
//! games' item ids overlap).
//!
//! "Already sent" works like the retail upload: a per-file set of `upload::owned_lot_fingerprint`
//! prints (auction id, seenAt, cancelledAt), so a lot the addon saw again, or saw cancelled, goes
//! up again. The site reads every character's newest seenAt in a batch as its roster time, so a
//! lot left out of a later roster is marked gone there.
//!
//! Each lot carries its character's faction when the same file knows it (the newest of the
//! character's gold readings and ledger rows that names one), so the site can match the lot to its
//! market even before it holds a reading of that character. Never on retail's lot body.

use crate::forever_ledger::PassportBody;
use crate::forever_runs::PurchasePass;
use crate::savedvars::{ClientPassport, LedgerData, OwnedLot};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// The site's cap on lots in one request (OWNED_LOTS_MAX_BATCH), the same as retail's.
pub const MAX_BATCH: usize = crate::upload::MAX_OWNED_LOT_BATCH;

// The site checks a batch whole: one lot it cannot read refuses all of them, and a refused batch is
// not sent again. So a lot outside these bounds is left out here instead (routes/foreverOwnedLots.ts).
const MAX_SAFE_ID: i64 = 9_007_199_254_740_991;
const MAX_ITEM_ID: i64 = 2_147_483_647;
const MAX_QUANTITY: i64 = 2_000_000_000;
const MAX_UNIT_PRICE: i64 = 1_000_000_000_000_000;

#[derive(Serialize)]
struct UploadBody<'a> {
    client: PassportBody<'a>,
    lots: Vec<WireLot>,
}

/// Retail's lot plus the character's faction, only when the file names one.
#[derive(Serialize)]
struct WireLot {
    #[serde(flatten)]
    lot: OwnedLot,
    #[serde(skip_serializing_if = "Option::is_none")]
    faction: Option<String>,
}

/// Each character's faction in the file: the newest gold reading or ledger row that names one
/// (`savedvars` keeps only "Alliance" and "Horde"), under the same "Name-Realm" the lots carry.
fn character_factions(data: &LedgerData) -> BTreeMap<String, String> {
    let mut newest: BTreeMap<String, (i64, String)> = BTreeMap::new();
    let readings = data.gold.iter().filter_map(|g| Some((g.character.as_str(), g.at, g.faction.as_ref()?)));
    let rows = data.entries.iter().filter_map(|e| Some((e.character.as_deref()?, e.at, e.faction.as_ref()?)));
    for (character, at, faction) in readings.chain(rows) {
        if newest.get(character).map_or(true, |(kept, _)| at > *kept) {
            newest.insert(character.to_string(), (at, faction.clone()));
        }
    }
    newest.into_iter().map(|(c, (_, f))| (c, f)).collect()
}

/// "Name-Realm" with a hyphen strictly inside, 3 to 96 characters as the site counts them.
fn readable_character(c: &str) -> bool {
    let len = c.encode_utf16().count();
    let hyphen = c.find('-');
    (3..=96).contains(&len) && hyphen.is_some_and(|at| at > 0 && at < c.len() - 1)
}

/// The lot as the site will take it, or None when the site would refuse it (and with it the batch).
/// A time that is not a positive number is left out rather than costing the lot.
fn wire_lot(l: &OwnedLot) -> Option<OwnedLot> {
    let ok = (1..=MAX_SAFE_ID).contains(&l.auction_id)
        && (1..=MAX_ITEM_ID).contains(&l.item_id)
        && (1..=MAX_QUANTITY).contains(&l.quantity)
        && (1..=MAX_UNIT_PRICE).contains(&l.unit_price)
        && l.seen_at > 0
        && readable_character(&l.character);
    ok.then(|| OwnedLot {
        expires_at: l.expires_at.filter(|t| *t > 0),
        cancelled_at: l.cancelled_at.filter(|t| *t > 0),
        ..l.clone()
    })
}

/// What goes up from one file: one row per auction id (the newest seenAt; the site refuses a batch
/// that names an auction twice), only lots the site can read, only prints not yet sent. Oldest
/// seenAt first, so a batch cut at the cap holds whole older rosters.
fn pending_lots<'a>(lots: &'a [OwnedLot], sent: &BTreeSet<String>) -> Vec<&'a OwnedLot> {
    let mut by_id: BTreeMap<i64, &OwnedLot> = BTreeMap::new();
    for lot in lots.iter().filter(|l| wire_lot(l).is_some()) {
        match by_id.get(&lot.auction_id) {
            Some(kept) if kept.seen_at >= lot.seen_at => {}
            _ => {
                by_id.insert(lot.auction_id, lot);
            }
        }
    }
    let mut out: Vec<&OwnedLot> =
        by_id.into_values().filter(|l| !sent.contains(&crate::upload::owned_lot_fingerprint(l))).collect();
    out.sort_by_key(|l| (l.seen_at, l.auction_id));
    out
}

/// What the site did with one batch.
#[derive(Debug, PartialEq)]
pub enum LotsOutcome {
    /// 200, or refused for good as it is (400, 409, 413): marked sent.
    Taken,
    /// 400 `timestamp_in_future`: this PC's clock runs ahead of the site's. Nothing is marked sent,
    /// so the lots go again on a later pass, once that time has come.
    Early,
    Unauthorized,
    Retry(String),
}

/// One request to the Forever owned-lots route under the pairing token and the file's passport.
pub async fn upload_lots(
    client: &reqwest::Client,
    base: &str,
    token: &str,
    passport: &ClientPassport,
    region_id: i64,
    lots: &[&OwnedLot],
    factions: &BTreeMap<String, String>,
) -> LotsOutcome {
    let body = UploadBody {
        client: PassportBody { interface: passport.interface, build: &passport.build, region_id },
        lots: lots
            .iter()
            .filter_map(|l| wire_lot(l))
            .map(|lot| WireLot { faction: factions.get(&lot.character).cloned(), lot })
            .collect(),
    };
    let sent = client
        .post(format!("{base}/v1/forever/owned-lots"))
        .bearer_auth(token)
        .header(crate::upload::CLIENT_HEADER, passport.header_value())
        .json(&body)
        .send()
        .await;
    match sent {
        Err(e) => LotsOutcome::Retry(crate::sync::describe_err(&e, crate::sync::CLIENT_TIMEOUT)),
        Ok(res) => match res.status().as_u16() {
            200 => LotsOutcome::Taken,
            400 => {
                let body = res.text().await.unwrap_or_default();
                if body.contains("timestamp_in_future") {
                    LotsOutcome::Early
                } else {
                    LotsOutcome::Taken
                }
            }
            // 409: the beta, China's servers or another game. 413: over the site's size cap. Either
            // way this batch will never be taken as it is, so resending it every tick helps nobody.
            409 | 413 => LotsOutcome::Taken,
            401 => LotsOutcome::Unauthorized,
            code => LotsOutcome::Retry(format!("status {code}")),
        },
    }
}

/// One file's pass: read it, forget prints it no longer holds, send what is new in batches.
pub async fn send_owned_lots(
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
    let present: BTreeSet<String> = data.owned_lots.iter().map(crate::upload::owned_lot_fingerprint).collect();
    sent.retain(|p| present.contains(p));

    let pending = pending_lots(&data.owned_lots, sent);
    let factions = character_factions(&data);
    let mut taken = 0usize;
    for batch in pending.chunks(MAX_BATCH) {
        match upload_lots(client, base, token, passport, region_id, batch, &factions).await {
            LotsOutcome::Taken => {
                sent.extend(batch.iter().map(|l| crate::upload::owned_lot_fingerprint(l)));
                taken += batch.len();
            }
            LotsOutcome::Early => {
                logger.info(&format!("forever auctions from {}: dated ahead of the site's clock, sent again later", file.display()));
                return PurchasePass::Incomplete;
            }
            LotsOutcome::Unauthorized => return PurchasePass::Unauthorized,
            LotsOutcome::Retry(why) => {
                logger.error(&format!("forever auctions from {}: will retry ({why})", file.display()));
                return PurchasePass::Incomplete;
            }
        }
    }
    if taken > 0 {
        logger.info(&format!("forever auctions from {}: sent ({taken} lots)", file.display()));
    }
    PurchasePass::Complete
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forever::tests::{answer, serve_forever, serve_once};

    const FILE: &str = r#"GoldCapDB = { client = { interface = 16001, build = "1.60.1.70938", regionId = 3 }, ownedLots = {
      { auctionID = 101, itemID = 2589, isCommodity = true, quantity = 20, unitPrice = 1500, char = "Alice-CAMWOW EU5 Realm 7 PvE", region = "eu", seenAt = 1794653802, expiresAt = 1794680006 },
      { auctionID = 102, itemID = 9765, isCommodity = false, quantity = 1, unitPrice = 700, char = "Alice-CAMWOW EU5 Realm 7 PvE", region = "eu", seenAt = 1794653802, expiresAt = 1794702825, cancelledAt = 1794654736 },
      { auctionID = 103, itemID = 4461, isCommodity = true, quantity = 3, unitPrice = 221, char = "Bob-CAMWOW EU5 Realm 7 PvE", region = "eu", seenAt = 1794667452 },
    } }"#;
    const REAL: &str = include_str!("../tests/fixtures/GoldCap-forever-lots.lua");

    fn write(dir: &Path, lua: &str) -> std::path::PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let file = dir.join("GoldCap.lua");
        std::fs::write(&file, lua).unwrap();
        file
    }

    fn body_of(request: &str) -> serde_json::Value {
        serde_json::from_str(request.split("\r\n\r\n").nth(1).unwrap()).unwrap()
    }

    fn setup(name: &str, lua: &str) -> (std::path::PathBuf, std::path::PathBuf, crate::logging::Logger) {
        let dir = std::env::temp_dir().join(format!("goldcap-forever-lots-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let file = write(&dir, lua);
        let logger = crate::logging::Logger::new(&dir.join("logs")).unwrap();
        (dir, file, logger)
    }

    #[tokio::test]
    async fn three_lots_go_up_once_with_the_forever_passport() {
        let (dir, file, logger) = setup("three", FILE);
        let mut sent = BTreeSet::new();
        let (base, seen) = serve_once(answer("200 OK", r#"{"accepted":3,"updated":0,"gone":0}"#));
        let pass = send_owned_lots(&crate::sync::build_client(), &base, "tok", &file, &mut sent, &logger).await;
        assert!(matches!(pass, PurchasePass::Complete));
        let request = seen.recv().unwrap();
        let lower = request.to_ascii_lowercase();
        assert!(lower.starts_with("post /v1/forever/owned-lots "), "{request}");
        assert!(lower.contains("x-goldcap-client: 16001/1.60.1.70938"));
        assert!(lower.contains("authorization: bearer tok"));
        assert_eq!(
            body_of(&request),
            serde_json::json!({
                "client": { "interface": 16001, "build": "1.60.1.70938", "regionId": 3 },
                "lots": [
                    { "auctionID": 101, "itemID": 2589, "isCommodity": true, "quantity": 20, "unitPrice": 1500, "expiresAt": 1794680006,
                      "char": "Alice-CAMWOW EU5 Realm 7 PvE", "seenAt": 1794653802 },
                    { "auctionID": 102, "itemID": 9765, "isCommodity": false, "quantity": 1, "unitPrice": 700, "expiresAt": 1794702825,
                      "char": "Alice-CAMWOW EU5 Realm 7 PvE", "seenAt": 1794653802, "cancelledAt": 1794654736 },
                    { "auctionID": 103, "itemID": 4461, "isCommodity": true, "quantity": 3, "unitPrice": 221,
                      "char": "Bob-CAMWOW EU5 Realm 7 PvE", "seenAt": 1794667452 }
                ]
            })
        );
        assert_eq!(sent.len(), 3);

        // Nothing new: no request at all (nothing listens on port 9).
        let pass = send_owned_lots(&crate::sync::build_client(), "http://127.0.0.1:9", "tok", &file, &mut sent, &logger).await;
        assert!(matches!(pass, PurchasePass::Complete));

        // The addon saw the roster again: every lot it still lists has a new seenAt and goes again;
        // the one it no longer lists stays as it was sent, and the site marks it gone.
        write(&dir, &FILE.replacen("seenAt = 1794653802", "seenAt = 1794700000", 1));
        let (base, seen) = serve_once(answer("200 OK", "{}"));
        send_owned_lots(&crate::sync::build_client(), &base, "tok", &file, &mut sent, &logger).await;
        let ids: Vec<i64> = body_of(&seen.recv().unwrap())["lots"].as_array().unwrap().iter().map(|l| l["auctionID"].as_i64().unwrap()).collect();
        assert_eq!(ids, vec![101]);
        assert_eq!(sent.len(), 3, "the old print of 101 is forgotten, its new one kept");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn the_owners_42_lots_go_up_in_one_request() {
        let (dir, file, logger) = setup("real", REAL);
        let mut sent = BTreeSet::new();
        let (base, seen) = serve_once(answer("200 OK", "{}"));
        let pass = send_owned_lots(&crate::sync::build_client(), &base, "tok", &file, &mut sent, &logger).await;
        assert!(matches!(pass, PurchasePass::Complete));
        let body = body_of(&seen.recv().unwrap());
        assert_eq!(body["client"], serde_json::json!({ "interface": 16001, "build": "1.60.1.70938", "regionId": 3 }));
        let lots = body["lots"].as_array().unwrap();
        assert_eq!(lots.len(), 42);
        assert!(lots.iter().all(|l| l.get("region").is_none() && l["char"] == "Tester-CAMWOW EU5 Realm 7 PvE"));
        assert_eq!(sent.len(), 42);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn a_retail_file_an_unstamped_one_and_one_without_a_region_send_nothing() {
        let (dir, _file, logger) = setup("skip", FILE);
        let mut sent = BTreeSet::new();
        for lua in [
            FILE.replace("interface = 16001, build = \"1.60.1.70938\"", "interface = 120100, build = \"12.1.0.69933\""),
            FILE.replace(", regionId = 3", ""),
            FILE.replace("client = { interface = 16001, build = \"1.60.1.70938\", regionId = 3 }, ", ""),
        ] {
            let file = write(&dir, &lua);
            let pass = send_owned_lots(&crate::sync::build_client(), "http://127.0.0.1:9", "tok", &file, &mut sent, &logger).await;
            assert!(matches!(pass, PurchasePass::Complete));
            assert!(sent.is_empty());
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn a_lot_the_site_would_refuse_is_left_out_and_the_rest_go_up() {
        let lua = FILE
            .replace(r#"char = "Bob-CAMWOW EU5 Realm 7 PvE""#, r#"char = "Bob""#)
            .replace("expiresAt = 1794680006", "expiresAt = 0");
        let (dir, file, logger) = setup("bad", &lua);
        let mut sent = BTreeSet::new();
        let (base, seen) = serve_once(answer("200 OK", "{}"));
        send_owned_lots(&crate::sync::build_client(), &base, "tok", &file, &mut sent, &logger).await;
        let body = body_of(&seen.recv().unwrap());
        let lots = body["lots"].as_array().unwrap();
        let ids: Vec<i64> = lots.iter().map(|l| l["auctionID"].as_i64().unwrap()).collect();
        assert_eq!(ids, vec![101, 102], "a character with no realm is not sent");
        assert!(lots[0].get("expiresAt").is_none(), "a zero expiry is left out, not the lot");
        assert_eq!(sent.len(), 2);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn one_auction_twice_goes_once_with_its_newest_reading() {
        let lua = FILE.replace("auctionID = 103", "auctionID = 101");
        let (dir, file, logger) = setup("dupe", &lua);
        let mut sent = BTreeSet::new();
        let (base, seen) = serve_once(answer("200 OK", "{}"));
        send_owned_lots(&crate::sync::build_client(), &base, "tok", &file, &mut sent, &logger).await;
        let body = body_of(&seen.recv().unwrap());
        let lots = body["lots"].as_array().unwrap();
        assert_eq!(lots.len(), 2);
        let first = lots.iter().find(|l| l["auctionID"] == 101).unwrap();
        assert_eq!(first["seenAt"], 1794667452);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn what_the_site_answers_decides_what_is_marked_sent() {
        let (dir, file, logger) = setup("answers", FILE);
        let client = crate::sync::build_client();

        // Refused for good (the beta, China, a bad body): marked sent, never sent again as it is.
        for status in ["409 Conflict", "400 Bad Request", "413 Payload Too Large"] {
            let mut sent = BTreeSet::new();
            let (base, _seen) = serve_once(answer(status, r#"{"error":"beta"}"#));
            let pass = send_owned_lots(&client, &base, "tok", &file, &mut sent, &logger).await;
            assert!(matches!(pass, PurchasePass::Complete), "{status}");
            assert_eq!(sent.len(), 3, "{status}");
        }

        // A clock ahead of the site's, a server error, too many requests, an older site: nothing
        // marked, the pass ends incomplete, so the file is read again next tick.
        for (status, body) in [
            ("400 Bad Request", r#"{"error":"timestamp_in_future"}"#),
            ("500 Internal Server Error", "{}"),
            ("429 Too Many Requests", r#"{"error":"rate_limited","retryAfterSec":60}"#),
            ("404 Not Found", "{}"),
        ] {
            let mut sent = BTreeSet::new();
            let (base, _seen) = serve_once(answer(status, body));
            let pass = send_owned_lots(&client, &base, "tok", &file, &mut sent, &logger).await;
            assert!(matches!(pass, PurchasePass::Incomplete), "{status}");
            assert!(sent.is_empty(), "{status}");
        }

        let mut sent = BTreeSet::new();
        let (base, _seen) = serve_once(answer("401 Unauthorized", r#"{"error":"unauthorized"}"#));
        let pass = send_owned_lots(&client, &base, "tok", &file, &mut sent, &logger).await;
        assert!(matches!(pass, PurchasePass::Unauthorized));
        assert!(sent.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn more_than_500_lots_go_in_batches_of_500() {
        let mut lua = String::from(r#"GoldCapDB = { client = { interface = 16001, build = "1.60.1.70938", regionId = 3 }, ownedLots = {"#);
        for i in 1..=620 {
            lua.push_str(&format!(
                r#"{{ auctionID = {i}, itemID = 2589, isCommodity = true, quantity = 1, unitPrice = 10, char = "Alice-Realm", region = "eu", seenAt = {} }},"#,
                1794600000 + i
            ));
        }
        lua.push_str("} }");
        let (dir, file, logger) = setup("batches", &lua);
        let mut sent = BTreeSet::new();
        let (base, seen) = serve_forever(vec![answer("200 OK", "{}"), answer("200 OK", "{}")]);
        let pass = send_owned_lots(&crate::sync::build_client(), &base, "tok", &file, &mut sent, &logger).await;
        assert!(matches!(pass, PurchasePass::Complete));
        let sizes: Vec<usize> = seen.try_iter().map(|r| body_of(&r)["lots"].as_array().unwrap().len()).collect();
        assert_eq!(sizes, vec![500, 120]);
        assert_eq!(sent.len(), 620);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn each_lot_carries_its_characters_newest_faction_from_the_same_file() {
        // Alice: a gold reading as Alliance, then a newer ledger row as Horde (the newest wins).
        // Bob: a gold reading with no faction, and one the site does not know: none is sent.
        let lua = FILE.replacen(
            "ownedLots = {",
            r#"gold = {
              { char = "Alice-CAMWOW EU5 Realm 7 PvE", region = "eu", copper = 100, at = 1794600000, faction = "Alliance" },
              { char = "Bob-CAMWOW EU5 Realm 7 PvE", region = "eu", copper = 100, at = 1794600000 },
              { char = "Bob-CAMWOW EU5 Realm 7 PvE", region = "eu", copper = 200, at = 1794600100, faction = "Neutral" },
            }, ledger = {
              { key = "k1", kind = "sale", source = "mail", qty = 1, total = 100, at = 1794650000, char = "Alice-CAMWOW EU5 Realm 7 PvE", region = "eu", faction = "Horde" },
            }, ownedLots = {"#,
            1,
        );
        let (dir, file, logger) = setup("faction", &lua);
        let mut sent = BTreeSet::new();
        let (base, seen) = serve_once(answer("200 OK", "{}"));
        send_owned_lots(&crate::sync::build_client(), &base, "tok", &file, &mut sent, &logger).await;
        let body = body_of(&seen.recv().unwrap());
        let factions: Vec<(i64, Option<&str>)> =
            body["lots"].as_array().unwrap().iter().map(|l| (l["auctionID"].as_i64().unwrap(), l.get("faction").and_then(|f| f.as_str()))).collect();
        assert_eq!(factions, vec![(101, Some("Horde")), (102, Some("Horde")), (103, None)]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_character_needs_a_realm_after_its_first_hyphen() {
        assert!(readable_character("Alice-CAMWOW EU5 Realm 7 PvE"));
        assert!(readable_character("Al-Azjol-Nerub"), "a realm with a hyphen");
        assert!(!readable_character("Alice"));
        assert!(!readable_character("-Realm"));
        assert!(!readable_character("Alice-"));
        assert!(!readable_character(&format!("A-{}", "r".repeat(95))));
    }
}
