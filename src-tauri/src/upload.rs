//! Pairing and ledger upload.
//!
//! Which rows have already been sent is tracked HERE, in the companion's own
//! config directory — never in the addon's SavedVariables, which WoW rewrites
//! at logout. Losing this file is harmless: the server is idempotent on the
//! addon's dedupe key, so the worst case is one redundant full re-send.

use crate::savedvars::{GoldPoint, LedgerData, LedgerEntry, LiveObservation};
use std::collections::HashSet;
use std::path::Path;

/// Must not exceed the API's own per-request cap (see routes/ledger.ts).
pub const MAX_BATCH: usize = 500;

pub const STATE_FILE_NAME: &str = "uploaded.json";

const CLAIM_URL: &str = "https://api.goldcap.gg/v1/companion/claim";
const UPLOAD_URL: &str = "https://api.goldcap.gg/v1/ledger/upload";

pub const LIVE_OBSERVATIONS_URL: &str = "https://api.goldcap.gg/v1/live-observations";
/// Must not exceed the API's own per-request cap (routes/live-observations.ts).
pub const MAX_OBSERVATION_BATCH: usize = 200;

/// What the Status screen shows for the ledger stage. Kept next to the dedupe
/// keys in the same file so a restart does not reset the counter to zero.
#[derive(Debug, Default, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadStats {
    pub total_sent: u64,
    pub last_upload_at: Option<i64>,
    /// Rows read out of SavedVariables that the server has not accepted yet.
    pub pending: u64,
    pub last_error: Option<String>,
}

/// The on-disk shape. v1.0.0 wrote a bare array of keys; `load_from` still
/// accepts that and migrates it on the next successful write.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct PersistedState {
    #[serde(default)]
    keys: HashSet<String>,
    #[serde(default)]
    total_sent: u64,
    #[serde(default)]
    last_upload_at: Option<i64>,
    #[serde(default)]
    pending: u64,
    #[serde(default)]
    last_error: Option<String>,
}

#[derive(Debug, Default, Clone)]
pub struct UploadState {
    uploaded: HashSet<String>,
    stats: UploadStats,
}

impl UploadState {
    /// 8000 → 20000: live observations add up to 200 fingerprints per scan
    /// on top of ledger + gold; eviction is arbitrary, and evicting a
    /// LEDGER key causes harmless-but-wasteful re-uploads.
    pub const MAX_KEYS: usize = 20000;

    pub fn load_from(path: &Path) -> Self {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        // The two on-disk shapes are told apart by the first non-whitespace
        // byte, not by which one happens to parse first: v1.0.0 wrote a bare
        // JSON array of keys, the current format a JSON object. Trying
        // PersistedState (an object) first and falling back to a bare
        // HashSet on error is NOT unambiguous — serde's derived
        // Deserialize accepts a JSON array against any struct positionally
        // (visit_seq), so a legacy array can parse straight into
        // PersistedState if its elements happen to fit the struct's fields
        // in order. That only failed to bite here because `keys` (a
        // HashSet<String>) is field 0: a bare string element can never
        // satisfy it. Reorder the fields, or add one whose type a string
        // could satisfy, and the try-object-first approach would start
        // silently parsing legacy arrays as an object with every key
        // dropped — a full ledger re-upload. Branching on the leading byte
        // makes that impossible by construction instead of by luck.
        if text.trim_start().starts_with('[') {
            return match serde_json::from_str::<HashSet<String>>(&text) {
                Ok(keys) => Self { uploaded: keys, stats: UploadStats::default() },
                Err(_) => Self::default(),
            };
        }
        match serde_json::from_str::<PersistedState>(&text) {
            Ok(p) => Self {
                uploaded: p.keys,
                stats: UploadStats {
                    total_sent: p.total_sent,
                    last_upload_at: p.last_upload_at,
                    pending: p.pending,
                    last_error: p.last_error,
                },
            },
            Err(_) => Self::default(),
        }
    }

    pub fn save_to(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let persisted = PersistedState {
            keys: self.uploaded.clone(),
            total_sent: self.stats.total_sent,
            last_upload_at: self.stats.last_upload_at,
            pending: self.stats.pending,
            last_error: self.stats.last_error.clone(),
        };
        let text = serde_json::to_string(&persisted)?;
        std::fs::write(path, text)
    }

    pub fn stats(&self) -> &UploadStats {
        &self.stats
    }

    pub fn record_upload(&mut self, rows: u64, at: i64) {
        self.stats.total_sent += rows;
        self.stats.last_upload_at = Some(at);
    }

    pub fn set_pending(&mut self, pending: u64) {
        self.stats.pending = pending;
    }

    pub fn set_last_error(&mut self, error: Option<String>) {
        self.stats.last_error = error;
    }

    pub fn contains(&self, key: &str) -> bool {
        self.uploaded.contains(key)
    }

    // Only the tests below inspect the raw key set directly; production code
    // goes through `contains`/`stats`. Gated so a non-test build never carries
    // dead public API for clippy to flag.
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.uploaded.is_empty()
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.uploaded.len()
    }

    pub fn remember(&mut self, keys: &[String]) {
        for key in keys {
            self.uploaded.insert(key.clone());
        }
        while self.uploaded.len() > Self::MAX_KEYS {
            let Some(victim) = self.uploaded.iter().next().cloned() else {
                break;
            };
            self.uploaded.remove(&victim);
        }
    }

    pub fn remember_entries(&mut self, entries: &[&LedgerEntry]) {
        let keys: Vec<String> = entries.iter().map(|e| entry_fingerprint(e)).collect();
        self.remember(&keys);
    }

    pub fn remember_gold(&mut self, points: &[&GoldPoint]) {
        let keys: Vec<String> = points.iter().map(|g| gold_fingerprint(g)).collect();
        self.remember(&keys);
    }
}

/// The dedupe key PLUS every mutable field the server is allowed to correct.
/// Fingerprinting on the key alone would permanently skip the "Sale Pending"
/// maturation, which reuses the same key on purpose once the money lands.
///
/// `region` joined the list on 2026-08-28. The addon used to stamp rows with
/// the region of the last IMPORT rather than the one the character plays in,
/// and now repairs those rows in place (GC.Ledger.RepairCharacterRegions).
/// With region outside the fingerprint that repair could never reach the
/// server: same key, same money, so the companion skipped the row forever and
/// the website went on pairing sales against a market they never happened in.
///
/// Adding a field invalidates every key already on disk, so the next run does
/// one full re-send. That is the documented worst case for this file (see the
/// module header) and exactly what is wanted here: it is what carries the
/// correction up for players who already have bad rows stored.
fn entry_fingerprint(e: &LedgerEntry) -> String {
    format!(
        "{}|{}|{}|{}|{}|{}",
        e.key,
        e.total,
        e.cut,
        e.deposit,
        e.pending,
        e.region.as_deref().unwrap_or("")
    )
}

fn gold_fingerprint(g: &GoldPoint) -> String {
    format!("gold|{}|{}", g.character, g.at)
}

/// `live|` prefix so observation fingerprints share uploaded.json's one key
/// set with ledger (bare key) and gold (`gold|`) fingerprints safely.
/// An observation is immutable — item + scan time IS its identity.
fn observation_fingerprint(o: &LiveObservation) -> String {
    format!("live|{}|{}", o.item_id, o.scanned_at)
}

/// Rows worth sending: not yet fingerprinted, and stamped with the SAME
/// region the companion is configured for — a row scanned on a
/// wrong-region alt must not be filed under this config's realm slug.
fn pending_observations<'a>(
    data: &'a LedgerData,
    state: &UploadState,
    region: &str,
) -> Vec<&'a LiveObservation> {
    data.observations
        .iter()
        .filter(|o| o.region == region && !state.contains(&observation_fingerprint(o)))
        .collect()
}

pub fn pending<'a>(
    data: &'a LedgerData,
    state: &UploadState,
) -> (Vec<&'a LedgerEntry>, Vec<&'a GoldPoint>) {
    let entries = data
        .entries
        .iter()
        .filter(|e| !state.contains(&entry_fingerprint(e)))
        .collect();
    let gold = data
        .gold
        .iter()
        .filter(|g| !state.contains(&gold_fingerprint(g)))
        .collect();
    (entries, gold)
}

/// Trades a pairing code for a long-lived upload token.
pub async fn claim_code(
    client: &reqwest::Client,
    code: &str,
    label: &str,
) -> Result<String, String> {
    let res = client
        .post(CLAIM_URL)
        .json(&serde_json::json!({ "code": code, "label": label }))
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;
    if !res.status().is_success() {
        return Err("that code is not valid — get a fresh one on goldcap.gg/account".into());
    }
    let body: serde_json::Value = res.json().await.map_err(|e| e.to_string())?;
    body.get("token")
        .and_then(|t| t.as_str())
        .map(|t| t.to_string())
        .ok_or_else(|| "the server did not return a token".to_string())
}

/// Posts one batch. Returns Ok only when the server accepted it, so a failed
/// batch is retried on the next tick rather than being silently forgotten.
pub async fn upload_batch(
    client: &reqwest::Client,
    token: &str,
    entries: &[&LedgerEntry],
    gold: &[&GoldPoint],
) -> Result<(), String> {
    let res = client
        .post(UPLOAD_URL)
        .bearer_auth(token)
        .json(&serde_json::json!({ "entries": entries, "gold": gold }))
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;
    if !res.status().is_success() {
        return Err(format!("unexpected status {}", res.status()));
    }
    Ok(())
}

/// One upload pass: read every SavedVariables file under the configured WoW
/// install, diff against local state, and post what is new in server-sized
/// batches. Only batches the server actually accepted are marked sent, so a
/// failed one is retried on the next tick rather than lost.
///
/// Never returns an error: upload is a passenger on the price-sync tick and
/// must not be able to break it. An unpaired companion (empty token) is a
/// silent no-op that still reports whatever it uploaded in the past.
pub async fn upload_once(
    client: &reqwest::Client,
    token: &str,
    wow_retail_path: &Path,
    state_path: &Path,
    logger: &crate::logging::Logger,
) -> UploadStats {
    let mut state = UploadState::load_from(state_path);
    if token.trim().is_empty() {
        return state.stats().clone();
    }

    let before = state.stats().clone();
    let mut sent = 0usize;
    let mut failed = 0usize;
    let mut pending_after = 0u64;
    let mut last_error: Option<String> = None;

    for file in crate::savedvars::saved_variables_paths(wow_retail_path) {
        let Ok(source) = std::fs::read_to_string(&file) else {
            continue;
        };
        let data = match crate::savedvars::parse_saved_variables(&source) {
            Ok(data) => data,
            Err(e) => {
                // A file we cannot parse is reported once and skipped; it must
                // never stop the loop or the other account's ledger.
                logger.error(&format!("could not read {}: {e}", file.display()));
                last_error = Some(format!("could not read {}", file.display()));
                continue;
            }
        };

        let (entries, gold) = pending(&data, &state);
        if !entries.is_empty() || !gold.is_empty() {
            // Gold rides along with the first batch; it is small and immutable.
            let mut gold_to_send: &[&GoldPoint] = &gold;
            let batches: Vec<&[&LedgerEntry]> = if entries.is_empty() {
                vec![&[]]
            } else {
                entries.chunks(MAX_BATCH).collect()
            };

            for batch in batches {
                match upload_batch(client, token, batch, gold_to_send).await {
                    Ok(()) => {
                        state.remember_entries(batch);
                        state.remember_gold(gold_to_send);
                        sent += batch.len();
                        gold_to_send = &[];
                    }
                    Err(e) => {
                        failed += 1;
                        logger.error(&format!("ledger upload failed: {e}"));
                        last_error = Some(e);
                        break; // retry this file's remainder next tick
                    }
                }
            }
        }

        // Recount against the updated state so "queue empty" is a fact, not a
        // hope. `data` is already in memory, so this costs no I/O.
        let (still_entries, still_gold) = pending(&data, &state);
        pending_after += (still_entries.len() + still_gold.len()) as u64;
    }

    state.set_pending(pending_after);
    state.set_last_error(last_error);
    if sent > 0 {
        state.record_upload(sent as u64, crate::luafile::now_unix());
    }

    if sent > 0 || *state.stats() != before {
        if let Err(e) = state.save_to(state_path) {
            // The rows did land; only our note of it failed. Next tick re-sends
            // them and the server absorbs the duplicates.
            logger.error(&format!("could not persist upload state: {e}"));
        }
    }
    if sent > 0 {
        logger.info(&format!("uploaded {sent} ledger rows"));
    }
    if failed > 0 {
        logger.error(&format!("{failed} ledger batches deferred to the next tick"));
    }

    state.stats().clone()
}

/// Live observations ride the same passenger rule as the ledger upload:
/// logger-only reporting, never an error return, so a bad batch cannot turn
/// a good price sync red. An unpaired companion (empty token) or an
/// unconfigured realm is a silent no-op. Returns rows accepted this pass.
pub async fn upload_observations_once(
    client: &reqwest::Client,
    token: &str,
    region: &str,
    realm_slug: &str,
    wow_retail_path: &Path,
    state_path: &Path,
    logger: &crate::logging::Logger,
) -> usize {
    if token.trim().is_empty() || realm_slug.trim().is_empty() {
        return 0;
    }

    let mut state = UploadState::load_from(state_path);
    let mut accepted = 0usize;

    for file in crate::savedvars::saved_variables_paths(wow_retail_path) {
        let Ok(source) = std::fs::read_to_string(&file) else {
            continue;
        };
        let Ok(data) = crate::savedvars::parse_saved_variables(&source) else {
            continue;
        };

        let rows = pending_observations(&data, &state, region);

        let wrong_region = data.observations.iter().filter(|o| o.region != region).count();
        if wrong_region > 0 {
            logger.info(&format!(
                "live observations: {wrong_region} rows from another region dropped ({})",
                file.display()
            ));
        }

        for batch in rows.chunks(MAX_OBSERVATION_BATCH) {
            let body = serde_json::json!({
                "region": region,
                "realmSlug": realm_slug,
                "observations": batch,
            });
            let sent = client
                .post(LIVE_OBSERVATIONS_URL)
                .bearer_auth(token)
                .json(&body)
                .send()
                .await;
            match sent {
                Ok(res) if res.status().is_success() => {
                    let keys: Vec<String> =
                        batch.iter().map(|o| observation_fingerprint(o)).collect();
                    state.remember(&keys);
                    accepted += batch.len();
                }
                Ok(res) => {
                    logger.error(&format!("live observations refused: {}", res.status()));
                    break; // retry this file's remainder next tick
                }
                Err(e) => {
                    logger.error(&format!("live observations failed: {e}"));
                    break; // retry this file's remainder next tick
                }
            }
        }
    }

    // Persisted once at the end of the whole pass -- the same granularity
    // upload_once uses for the ledger leg, not per-batch: a mid-pass crash
    // just re-sends what never made it to disk, and the server absorbs the
    // duplicate idempotently on the fingerprint.
    if accepted > 0 {
        if let Err(e) = state.save_to(state_path) {
            logger.error(&format!("could not persist live observation state: {e}"));
        }
        logger.info(&format!("live observations sent: {accepted}"));
    }

    accepted
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::savedvars::{parse_saved_variables, GoldPoint, LedgerData, LedgerEntry, LiveObservation};

    const REAL_FILE: &str = include_str!("../tests/fixtures/GoldCap.lua");

    fn entry(key: &str) -> LedgerEntry {
        LedgerEntry {
            key: key.into(),
            kind: "sale".into(),
            source: "mail".into(),
            item_id: None,
            item_name: Some("Ironclaw Ore".into()),
            qty: 20,
            total: 500_000,
            cut: 25_000,
            deposit: 1_000,
            pending: false,
            mv: None,
            at: 1_785_600_000,
            character: Some("Testchar-Dentarg".into()),
            region: Some("eu".into()),
            decision_version: None,
            decision_status: None,
            decision_reasons: None,
            stress_unit: None,
            expected_profit: None,
            recommended_quantity: None,
            source_at: None,
        }
    }

    fn gold(at: i64) -> GoldPoint {
        GoldPoint {
            character: "Testchar-Dentarg".into(),
            region: Some("eu".into()),
            copper: 1,
            at,
        }
    }

    fn observation(item_id: i64, scanned_at: i64) -> LiveObservation {
        LiveObservation {
            item_id,
            region: "eu".into(),
            scanned_at,
            min_unit: 1000,
            listings: None,
            total_qty: None,
            levels: None,
        }
    }

    #[test]
    fn observation_fingerprints_key_on_item_and_scan_time_with_their_own_prefix() {
        let a = observation_fingerprint(&observation(42, 5000));
        assert_eq!(a, "live|42|5000");
        assert_ne!(a, observation_fingerprint(&observation(42, 5001)));
    }

    #[test]
    fn pending_observations_drops_already_sent_and_wrong_region_rows() {
        let mut state = UploadState::default();
        state.remember(&["live|42|5000".to_string()]);
        let data = LedgerData {
            observations: vec![
                observation(42, 5000), // already sent
                observation(7, 6000),  // fresh
                LiveObservation { region: "us".into(), ..observation(9, 6100) }, // wrong region
            ],
            ..Default::default()
        };
        let pending = pending_observations(&data, &state, "eu");
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].item_id, 7);
    }

    #[test]
    fn observation_batches_never_exceed_the_server_cap() {
        let rows: Vec<LiveObservation> = (0..450).map(|i| observation(i, 5000 + i)).collect();
        let refs: Vec<&LiveObservation> = rows.iter().collect();
        let batches: Vec<_> = refs.chunks(MAX_OBSERVATION_BATCH).collect();
        assert!(batches.iter().all(|b| b.len() <= 200));
        assert_eq!(batches.len(), 3);
    }

    #[tokio::test]
    async fn an_unpaired_observation_pass_sends_nothing() {
        let dir = std::env::temp_dir().join(format!("goldcap-liveobs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let client = reqwest::Client::new();
        let logger = crate::logging::Logger::new(&dir).unwrap();
        let sent = upload_observations_once(
            &client,
            "  ",
            "eu",
            "dentarg",
            std::path::Path::new("/nonexistent"),
            &dir.join("uploaded.json"),
            &logger,
        )
        .await;
        assert_eq!(sent, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn everything_is_pending_on_a_first_run() {
        let data = LedgerData {
            entries: vec![entry("a"), entry("b")],
            gold: vec![gold(1)],
            observations: vec![],
        };
        let (entries, points) = pending(&data, &UploadState::default());
        assert_eq!(entries.len(), 2);
        assert_eq!(points.len(), 1);
    }

    #[test]
    fn a_real_file_is_entirely_pending_the_first_time() {
        let data = parse_saved_variables(REAL_FILE).unwrap();
        let (entries, points) = pending(&data, &UploadState::default());
        assert_eq!(entries.len(), 2);
        assert_eq!(points.len(), 5);
    }

    #[test]
    fn a_real_file_re_read_after_upload_sends_nothing() {
        // The sync loop re-reads the same file every tick; a second pass must
        // be silent or the server would take the whole ledger every 30 minutes.
        let data = parse_saved_variables(REAL_FILE).unwrap();
        let mut state = UploadState::default();
        {
            let (entries, points) = pending(&data, &state);
            state.remember_entries(&entries);
            state.remember_gold(&points);
        }
        let (entries, points) = pending(&data, &state);
        assert!(entries.is_empty());
        assert!(points.is_empty());
    }

    #[test]
    fn already_uploaded_entries_are_skipped() {
        let mut state = UploadState::default();
        state.remember_entries(&[&entry("a")]);
        let data = LedgerData {
            entries: vec![entry("a"), entry("b")],
            gold: vec![],
            observations: vec![],
        };
        let (entries, _) = pending(&data, &state);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].key, "b");
    }

    #[test]
    fn a_matured_sale_pending_row_is_sent_again_under_the_same_key() {
        // The maturation deliberately keeps the key, so fingerprinting on the
        // key alone would strand it as pending forever. The server's upsert is
        // what makes the resend correct; this is what makes it happen at all.
        let mut state = UploadState::default();
        let mut still_pending = entry("a");
        still_pending.pending = true;
        still_pending.total = 0;
        state.remember_entries(&[&still_pending]);

        let data = LedgerData {
            entries: vec![entry("a")], // pending=false, total=500_000
            gold: vec![],
            observations: vec![],
        };
        let (entries, _) = pending(&data, &state);
        assert_eq!(entries.len(), 1, "a row whose contents changed must be re-sent");
    }

    #[test]
    fn a_row_whose_region_was_repaired_is_sent_again_under_the_same_key() {
        // The addon rewrites the region of rows it stamped from the imported
        // snapshot instead of from the client (Core/Ledger.lua's
        // RepairCharacterRegions). Key and money are untouched by that repair,
        // so leaving region out of the fingerprint stranded the correction on
        // the player's disk while the website kept pairing FIFO lots against a
        // market the sale never happened in.
        let mut state = UploadState::default();
        let mut mislabelled = entry("a");
        mislabelled.region = Some("kr".into());
        state.remember_entries(&[&mislabelled]);

        let data = LedgerData {
            entries: vec![entry("a")], // region repaired back to eu
            gold: vec![],
            observations: vec![],
        };
        let (entries, _) = pending(&data, &state);
        assert_eq!(entries.len(), 1, "a repaired region must reach the server");
    }

    #[test]
    fn gold_points_dedupe_on_character_and_timestamp() {
        let mut state = UploadState::default();
        state.remember_gold(&[&gold(1)]);
        let data = LedgerData {
            entries: vec![],
            gold: vec![gold(1), gold(2)],
            observations: vec![],
        };
        let (_, points) = pending(&data, &state);
        assert_eq!(points.len(), 1);
        assert_eq!(points[0].at, 2);
    }

    #[test]
    fn state_round_trips_through_disk() {
        let dir = std::env::temp_dir().join(format!("goldcap-upload-state-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join(STATE_FILE_NAME);

        let mut state = UploadState::default();
        state.remember(&["a".to_string(), "b".to_string()]);
        state.save_to(&path).unwrap();

        let loaded = UploadState::load_from(&path);
        assert!(loaded.contains("a"));
        assert!(loaded.contains("b"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_missing_state_file_is_an_empty_state_not_an_error() {
        // Losing this file must degrade to "re-upload everything", which the
        // server absorbs idempotently — never to a crash or a stuck sync.
        let state = UploadState::load_from(Path::new("/definitely/not/here.json"));
        assert!(state.is_empty());
    }

    #[test]
    fn a_corrupt_state_file_is_an_empty_state_not_an_error() {
        let dir = std::env::temp_dir().join(format!("goldcap-upload-junk-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(STATE_FILE_NAME);
        std::fs::write(&path, "{not json").unwrap();

        assert!(UploadState::load_from(&path).is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn state_is_capped_so_it_cannot_grow_without_bound() {
        let mut state = UploadState::default();
        let keys: Vec<String> = (0..UploadState::MAX_KEYS + 100)
            .map(|i| i.to_string())
            .collect();
        state.remember(&keys);
        assert!(state.len() <= UploadState::MAX_KEYS);
    }

    #[test]
    fn batches_never_exceed_the_server_cap() {
        let data = LedgerData {
            entries: (0..1200).map(|i| entry(&i.to_string())).collect(),
            gold: vec![],
            observations: vec![],
        };
        let (entries, _) = pending(&data, &UploadState::default());
        let batches: Vec<_> = entries.chunks(MAX_BATCH).collect();
        assert!(batches.iter().all(|b| b.len() <= MAX_BATCH));
        assert_eq!(batches.iter().map(|b| b.len()).sum::<usize>(), 1200);
    }

    #[test]
    fn the_old_bare_array_state_file_still_loads() {
        // v1.0.0 wrote a bare JSON array of keys. Failing to read it would
        // re-upload the user's entire ledger on first launch of this build.
        let dir = std::env::temp_dir().join(format!("goldcap-upload-legacy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(STATE_FILE_NAME);
        std::fs::write(&path, r#"["a","b"]"#).unwrap();

        let state = UploadState::load_from(&path);
        assert!(state.contains("a"));
        assert!(state.contains("b"));
        assert_eq!(state.stats().total_sent, 0, "the old format carried no counters");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_single_element_legacy_array_still_round_trips_its_key() {
        // The specific case the old try-object-first guard only got right by
        // luck: PersistedState's field 0 is `keys: HashSet<String>`, and a
        // bare string element can never satisfy that type, so the object
        // parse happened to fail and fall through. The leading-byte branch
        // gets this right on purpose instead.
        let dir = std::env::temp_dir()
            .join(format!("goldcap-upload-legacy-one-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(STATE_FILE_NAME);
        std::fs::write(&path, r#"["a"]"#).unwrap();

        let state = UploadState::load_from(&path);
        assert!(state.contains("a"));
        assert_eq!(state.len(), 1);
        assert_eq!(state.stats().total_sent, 0, "the old format carried no counters");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn stats_round_trip_through_disk_alongside_the_keys() {
        let dir = std::env::temp_dir().join(format!("goldcap-upload-stats-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join(STATE_FILE_NAME);

        let mut state = UploadState::default();
        state.remember(&["a".to_string()]);
        state.record_upload(7, 1_785_600_000);
        state.set_pending(3);
        state.save_to(&path).unwrap();

        let loaded = UploadState::load_from(&path);
        assert!(loaded.contains("a"));
        assert_eq!(loaded.stats().total_sent, 7);
        assert_eq!(loaded.stats().last_upload_at, Some(1_785_600_000));
        assert_eq!(loaded.stats().pending, 3);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn recorded_uploads_accumulate_across_passes() {
        let mut state = UploadState::default();
        state.record_upload(5, 100);
        state.record_upload(3, 200);
        assert_eq!(state.stats().total_sent, 8);
        assert_eq!(state.stats().last_upload_at, Some(200));
    }

    #[tokio::test]
    async fn an_unpaired_pass_returns_the_stored_stats_untouched() {
        let dir = std::env::temp_dir().join(format!("goldcap-upload-unpaired-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join(STATE_FILE_NAME);
        let mut state = UploadState::default();
        state.record_upload(42, 1_785_600_000);
        state.save_to(&path).unwrap();

        let logger = crate::logging::Logger::new(&dir).unwrap();
        let client = crate::sync::build_client();
        let stats =
            upload_once(&client, "", Path::new("/definitely/not/here"), &path, &logger).await;

        assert_eq!(stats.total_sent, 42, "an unpaired companion still shows its history");

        std::fs::remove_dir_all(&dir).ok();
    }
}
