//! Holdings: what each character holds, place by place (bags, bank, warband, mail, auctions,
//! guild bank), and the mail the player sent with items. The site uses it to take stock the player
//! no longer has off their ledger. This module holds what both games share and the retail pass;
//! `forever_holdings` is the WoW: Forever pass.
//!
//! "Already sent" is per file: the readAt of each "holder|place" reading the site took, and the
//! newest send time it took. A place read again goes up again; one the file no longer holds is
//! forgotten. Sends go up from that newest time on, so the one at the boundary goes again: the site
//! keeps the greatest time per item, so that is harmless.

use crate::savedvars::{HoldingReading, Holdings, MailSend};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

pub const STATE_FILE_NAME: &str = "holdings.json";
pub const RETAIL_ROUTE: &str = "/v1/ledger/holdings";
/// The site's caps on one request (HOLDINGS_MAX_READINGS, HOLDINGS_MAX_SENDS).
pub const MAX_READINGS: usize = 100;
pub const MAX_SENDS: usize = 500;
/// Readings in one request stop short of this, well under the site's 2 MiB body cap.
const MAX_READING_BYTES: usize = 1024 * 1024;

/// What the site has taken from one file.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HoldingsSent {
    /// "holder|place" to the readAt the site took.
    pub readings: BTreeMap<String, i64>,
    /// The newest send time the site took.
    pub sends_at: i64,
}

/// `holdings.json` in the config dir: the retail pass's `HoldingsSent`, by file.
#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HoldingsState {
    pub sent: BTreeMap<String, BTreeMap<String, i64>>,
    pub sends_at: BTreeMap<String, i64>,
}

impl HoldingsState {
    pub fn load_from(path: &Path) -> Self {
        std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    pub fn save_to(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        crate::luafile::write_atomic(path, &serde_json::to_string_pretty(self).map_err(std::io::Error::from)?)
    }

    fn take(&mut self, file: &str) -> HoldingsSent {
        HoldingsSent { readings: self.sent.remove(file).unwrap_or_default(), sends_at: self.sends_at.remove(file).unwrap_or(0) }
    }

    fn put(&mut self, file: String, sent: HoldingsSent) {
        if sent.sends_at > 0 {
            self.sends_at.insert(file.clone(), sent.sends_at);
        }
        if !sent.readings.is_empty() {
            self.sent.insert(file, sent.readings);
        }
    }
}

fn key(r: &HoldingReading) -> String {
    format!("{}|{}", r.holder, r.place)
}

/// One request's rows. `readings` is always there, even empty; `sends` only when there are some.
#[derive(Debug, Default, Serialize)]
pub(crate) struct Batch<'a> {
    pub(crate) readings: Vec<&'a HoldingReading>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) sends: Vec<&'a MailSend>,
}

impl Batch<'_> {
    pub(crate) fn rows(&self) -> usize {
        self.readings.len() + self.sends.len()
    }
}

/// What goes up from one file, in requests the site takes. Forgets the stamps of places the file no
/// longer holds. Nothing at all unless a reading is newer than its stamp or a send newer than the
/// newest taken: the boundary send alone is no reason for a request.
pub(crate) fn pending<'a>(h: &'a Holdings, sent: &mut HoldingsSent) -> Vec<Batch<'a>> {
    let present: BTreeSet<String> = h.readings.iter().map(key).collect();
    sent.readings.retain(|k, _| present.contains(k));
    let readings: Vec<&HoldingReading> =
        h.readings.iter().filter(|r| sent.readings.get(&key(r)).map_or(true, |at| r.read_at > *at)).collect();
    if readings.is_empty() && !h.sends.iter().any(|s| s.at > sent.sends_at) {
        return Vec::new();
    }
    let sends: Vec<&MailSend> = h.sends.iter().filter(|s| s.at >= sent.sends_at).collect();

    let mut batches: Vec<Batch> = Vec::new();
    let mut bytes = 0;
    for r in readings {
        let size = serde_json::to_vec(r).map_or(0, |b| b.len());
        match batches.last_mut() {
            Some(b) if b.readings.len() < MAX_READINGS && bytes + size <= MAX_READING_BYTES => b.readings.push(r),
            _ => {
                bytes = 0;
                batches.push(Batch { readings: vec![r], sends: Vec::new() });
            }
        }
        bytes += size;
    }
    for (i, chunk) in sends.chunks(MAX_SENDS).enumerate() {
        match batches.get_mut(i) {
            Some(b) => b.sends = chunk.to_vec(),
            None => batches.push(Batch { readings: Vec::new(), sends: chunk.to_vec() }),
        }
    }
    batches
}

/// Marks one batch the site has had its say about as sent.
pub(crate) fn stamp(sent: &mut HoldingsSent, batch: &Batch) {
    for r in &batch.readings {
        sent.readings.insert(key(r), r.read_at);
    }
    sent.sends_at = batch.sends.iter().map(|s| s.at).fold(sent.sends_at, i64::max);
}

/// The retail pass, after the ledger: logger-only, like the other passengers. An unpaired
/// companion is a silent no-op. Any answer but a 2xx stops the file and stamps nothing of that
/// request, so an older site (404) is asked again next tick. Returns the rows the site accepted.
pub async fn upload_holdings_once(
    client: &reqwest::Client,
    base: &str,
    token: &str,
    wow_retail_path: &Path,
    state_path: &Path,
    logger: &crate::logging::Logger,
) -> usize {
    if token.trim().is_empty() {
        return 0;
    }
    let mut state = HoldingsState::load_from(state_path);
    let mut changed = false;
    let mut accepted = 0usize;

    for file in crate::savedvars::saved_variables_paths(wow_retail_path) {
        let Ok(source) = std::fs::read_to_string(&file) else { continue };
        let Ok(Some(holdings)) = crate::savedvars::parse_holdings(&source) else { continue };
        if !holdings.client.as_ref().map_or(true, |p| p.is_retail()) {
            continue;
        }
        let file_key = file.to_string_lossy().into_owned();
        let mut sent = state.take(&file_key);
        let before = sent.clone();
        for batch in pending(&holdings, &mut sent) {
            let mut req = client.post(format!("{base}{RETAIL_ROUTE}")).bearer_auth(token);
            if let Some(p) = &holdings.client {
                req = req.header(crate::upload::CLIENT_HEADER, p.header_value());
            }
            match req.json(&batch).send().await {
                Ok(res) if res.status().is_success() => {
                    let answer: serde_json::Value = res.json().await.unwrap_or_default();
                    accepted += answer["accepted"].as_u64().unwrap_or(0) as usize;
                    stamp(&mut sent, &batch);
                }
                Ok(res) => {
                    logger.error(&format!("holdings from {}: will retry (status {})", file.display(), res.status().as_u16()));
                    break;
                }
                Err(e) => {
                    logger.error(&format!(
                        "holdings from {}: will retry ({})",
                        file.display(),
                        crate::sync::describe_err(&e, crate::sync::CLIENT_TIMEOUT)
                    ));
                    break;
                }
            }
        }
        changed |= sent != before;
        state.put(file_key, sent);
    }

    if changed {
        if let Err(e) = state.save_to(state_path) {
            logger.error(&format!("could not persist holdings state: {e}"));
        }
        logger.info(&format!("holdings sent: {accepted} accepted"));
    }
    accepted
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forever::tests::{answer, serve_once};
    use crate::forever_runs::PurchasePass;
    use std::path::PathBuf;
    use std::time::Duration;

    pub(crate) const FILE: &str = include_str!("../tests/fixtures/GoldCap-holdings.lua");

    pub(crate) fn body_of(request: &str) -> serde_json::Value {
        serde_json::from_str(request.split("\r\n\r\n").nth(1).unwrap()).unwrap()
    }

    /// A temp WoW folder with one account's GoldCap.lua; hands back (dir, file, state path, logger).
    fn setup(name: &str, lua: &str) -> (PathBuf, PathBuf, PathBuf, crate::logging::Logger) {
        let dir = std::env::temp_dir().join(format!("goldcap-holdings-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let sv = dir.join("WTF/Account/A/SavedVariables");
        std::fs::create_dir_all(&sv).unwrap();
        std::fs::write(sv.join("GoldCap.lua"), lua).unwrap();
        let logger = crate::logging::Logger::new(&dir.join("logs")).unwrap();
        (dir.clone(), sv.join("GoldCap.lua"), dir.join("state").join(STATE_FILE_NAME), logger)
    }

    fn places(body: &serde_json::Value) -> Vec<String> {
        body["readings"].as_array().unwrap().iter().map(|r| format!("{}|{}", r["holder"].as_str().unwrap(), r["place"].as_str().unwrap())).collect()
    }

    fn forever(lua: &str) -> String {
        lua.replacen("[\"interface\"] = 120100,", "[\"interface\"] = 16001, [\"regionId\"] = 3,", 1).replacen("12.1.0.69933", "1.60.1.70938", 1)
    }

    #[tokio::test]
    async fn holdings_404_is_retried_and_nothing_is_stamped() {
        let (dir, file, state_path, logger) = setup("404", FILE);
        let client = crate::sync::build_client();
        let (base, seen) = serve_once(answer("404 Not Found", r#"{"error":"not_found"}"#));
        assert_eq!(upload_holdings_once(&client, &base, "tok", &dir, &state_path, &logger).await, 0);
        assert!(seen.recv().unwrap().starts_with("POST /v1/ledger/holdings "));
        assert_eq!(HoldingsState::load_from(&state_path), HoldingsState::default());

        std::fs::write(&file, forever(FILE)).unwrap();
        let mut sent = HoldingsSent::default();
        let (base, seen) = serve_once(answer("404 Not Found", r#"{"error":"not_found"}"#));
        let pass = crate::forever_holdings::send_holdings(&client, &base, "tok", &file, &mut sent, &logger).await;
        assert!(matches!(pass, PurchasePass::Incomplete));
        assert!(seen.recv().unwrap().starts_with("POST /v1/forever/ledger/holdings "));
        assert_eq!(sent, HoldingsSent::default());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn holdings_200_stamps_and_the_next_tick_sends_nothing() {
        let (dir, _file, state_path, logger) = setup("200", FILE);
        let client = crate::sync::build_client();
        let (base, seen) = serve_once(answer("200 OK", r#"{"accepted":10,"skipped":0,"sends":1}"#));
        assert_eq!(upload_holdings_once(&client, &base, "tok", &dir, &state_path, &logger).await, 10);
        let request = seen.recv().unwrap();
        assert!(request.to_ascii_lowercase().contains("x-goldcap-client: 120100/12.1.0.69933"));
        assert!(request.to_ascii_lowercase().contains("authorization: bearer tok"));
        let body = body_of(&request);
        assert_eq!(places(&body).len(), 10);
        assert_eq!(body["sends"].as_array().unwrap().len(), 1);
        assert_eq!(body["readings"][1], serde_json::json!({ "holder": "Aiyana-Dentarg", "place": "bags", "region": "eu", "readAt": 1794600000, "full": true, "items": [[2589, 40], [190320, 12]] }));

        let state = HoldingsState::load_from(&state_path);
        let key = state.sent.keys().next().unwrap().clone();
        assert_eq!(state.sent[&key].len(), 10);
        assert_eq!(state.sent[&key]["Goldmakers-Dentarg|guild"], 1794580000);
        assert_eq!(state.sends_at[&key], 1794597000);

        let (base, seen) = serve_once(answer("200 OK", "{}"));
        assert_eq!(upload_holdings_once(&client, &base, "tok", &dir, &state_path, &logger).await, 0);
        assert!(seen.recv_timeout(Duration::from_millis(300)).is_err(), "nothing new, no request");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn a_newer_read_of_one_place_sends_only_that_place() {
        let (dir, file, state_path, logger) = setup("newer", FILE);
        let client = crate::sync::build_client();
        let (base, _seen) = serve_once(answer("200 OK", r#"{"accepted":10,"skipped":0,"sends":1}"#));
        upload_holdings_once(&client, &base, "tok", &dir, &state_path, &logger).await;

        // Bjorn's bags read again; Bjorn's bank gone from the file.
        let lua = FILE.replacen("[\"at\"] = 1794601000,", "[\"at\"] = 1794700000,", 1).replacen(
            "[\"bank\"] = {\n\t\t\t\t\t[\"at\"] = 1794591000,",
            "[\"bonk\"] = {\n\t\t\t\t\t[\"at\"] = 1794591000,",
            1,
        );
        std::fs::write(&file, lua).unwrap();
        let (base, seen) = serve_once(answer("200 OK", r#"{"accepted":1,"skipped":0,"sends":1}"#));
        upload_holdings_once(&client, &base, "tok", &dir, &state_path, &logger).await;
        let body = body_of(&seen.recv().unwrap());
        assert_eq!(places(&body), vec!["Bjorn-Dentarg|bags"]);
        assert_eq!(body["sends"].as_array().unwrap().len(), 1, "the boundary send goes again");

        let state = HoldingsState::load_from(&state_path);
        let stamps = state.sent.values().next().unwrap();
        assert_eq!(stamps.len(), 9, "the gone place's stamp is dropped");
        assert_eq!(stamps["Bjorn-Dentarg|bags"], 1794700000);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn a_file_without_holdings_makes_no_request() {
        let (dir, file, state_path, logger) = setup("none", include_str!("../tests/fixtures/GoldCap.lua"));
        let client = crate::sync::build_client();
        let (base, seen) = serve_once(answer("200 OK", "{}"));
        assert_eq!(upload_holdings_once(&client, &base, "tok", &dir, &state_path, &logger).await, 0);
        assert!(seen.recv_timeout(Duration::from_millis(300)).is_err());
        assert!(!state_path.exists());

        std::fs::write(&file, include_str!("../tests/fixtures/GoldCap-forever-lots.lua")).unwrap();
        let mut sent = HoldingsSent::default();
        let pass = crate::forever_holdings::send_holdings(&client, &base, "tok", &file, &mut sent, &logger).await;
        assert!(matches!(pass, PurchasePass::Complete));
        assert!(seen.recv_timeout(Duration::from_millis(300)).is_err());

        // Unpaired, or a Forever file in the retail folder: no request either.
        std::fs::write(&file, FILE).unwrap();
        assert_eq!(upload_holdings_once(&client, &base, " ", &dir, &state_path, &logger).await, 0);
        std::fs::write(&file, forever(FILE)).unwrap();
        assert_eq!(upload_holdings_once(&client, &base, "tok", &dir, &state_path, &logger).await, 0);
        assert!(seen.recv_timeout(Duration::from_millis(300)).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn many_readings_and_sends_go_in_requests_the_site_takes() {
        let reading = |i: usize| HoldingReading { holder: format!("C{i}-R"), place: "bags".into(), region: None, read_at: 1, full: true, items: vec![(1, 1)] };
        let send = |i: i64| MailSend { from: "A-R".into(), to: "B-R".into(), region: None, at: i, items: vec![(1, 1)] };
        let h = Holdings { readings: (0..250).map(reading).collect(), sends: (1..=1200).map(send).collect(), client: None };
        let batches = pending(&h, &mut HoldingsSent::default());
        let sizes: Vec<(usize, usize)> = batches.iter().map(|b| (b.readings.len(), b.sends.len())).collect();
        assert_eq!(sizes, vec![(100, 500), (100, 500), (50, 200)]);

        let big = HoldingReading { items: (1..=2000).map(|id| (id, 2_000_000_000)).collect(), ..reading(0) };
        let h = Holdings { readings: vec![big; 60], sends: Vec::new(), client: None };
        assert!(pending(&h, &mut HoldingsSent::default()).iter().all(|b| serde_json::to_vec(b).unwrap().len() < 2 * 1024 * 1024));
    }

    /// Run by hand against the local stand (token `stand-holdings-token`), never in CI:
    ///   GOLDCAP_E2E_API=http://localhost:8093 GOLDCAP_E2E_RETAIL_SV=... GOLDCAP_E2E_FOREVER_SV=... \
    ///   cargo test --locked -- --ignored holdings_e2e
    /// Each file is copied into a temp folder first: the game's own file is only read.
    #[tokio::test]
    #[ignore]
    async fn holdings_e2e_against_the_local_stand() {
        let base = std::env::var("GOLDCAP_E2E_API").expect("GOLDCAP_E2E_API");
        let client = crate::sync::build_client();
        let read = |var: &str| std::fs::read_to_string(std::env::var(var).expect(var)).unwrap();

        let (dir, _file, state_path, logger) = setup("e2e-retail", &read("GOLDCAP_E2E_RETAIL_SV"));
        let accepted = upload_holdings_once(&client, &base, "stand-holdings-token", &dir, &state_path, &logger).await;
        assert!(accepted > 0, "a 200 with accepted > 0 from the retail route");
        assert!(!HoldingsState::load_from(&state_path).sent.is_empty());

        let (dir, file, _state, logger) = setup("e2e-forever", &read("GOLDCAP_E2E_FOREVER_SV"));
        let mut sent = HoldingsSent::default();
        let pass = crate::forever_holdings::send_holdings(&client, &base, "stand-holdings-token", &file, &mut sent, &logger).await;
        assert!(matches!(pass, PurchasePass::Complete));
        assert!(!sent.readings.is_empty(), "the Forever route took the readings");
        let log = std::fs::read_to_string(dir.join("logs").join(crate::logging::LOG_FILE_NAME)).unwrap();
        assert!(log.contains(" accepted)") && !log.contains(", 0 accepted)"), "a 200 with accepted > 0: {log}");
        std::fs::remove_dir_all(&dir).ok();
    }
}
