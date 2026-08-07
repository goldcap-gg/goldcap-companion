//! Pairing and ledger upload.
//!
//! Which rows have already been sent is tracked HERE, in the companion's own
//! config directory — never in the addon's SavedVariables, which WoW rewrites
//! at logout. Losing this file is harmless: the server is idempotent on the
//! addon's dedupe key, so the worst case is one redundant full re-send.

use crate::savedvars::{GoldPoint, LedgerData, LedgerEntry};
use std::collections::HashSet;
use std::path::Path;

/// Must not exceed the API's own per-request cap (see routes/ledger.ts).
pub const MAX_BATCH: usize = 500;

pub const STATE_FILE_NAME: &str = "uploaded.json";

const CLAIM_URL: &str = "https://api.goldcap.gg/v1/companion/claim";
const UPLOAD_URL: &str = "https://api.goldcap.gg/v1/ledger/upload";

#[derive(Debug, Default, Clone)]
pub struct UploadState {
    uploaded: HashSet<String>,
}

impl UploadState {
    /// Roughly the addon's own ledger cap plus headroom. Bounded because this
    /// file is read and rewritten on every sync tick.
    pub const MAX_KEYS: usize = 8000;

    pub fn load_from(path: &Path) -> Self {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        Self {
            uploaded: serde_json::from_str(&text).unwrap_or_default(),
        }
    }

    pub fn save_to(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string(&self.uploaded)?;
        std::fs::write(path, text)
    }

    pub fn contains(&self, key: &str) -> bool {
        self.uploaded.contains(key)
    }

    pub fn is_empty(&self) -> bool {
        self.uploaded.is_empty()
    }

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

/// The dedupe key PLUS the mutable money fields. Fingerprinting on the key
/// alone would permanently skip the "Sale Pending" maturation, which reuses
/// the same key on purpose once the money lands.
fn entry_fingerprint(e: &LedgerEntry) -> String {
    format!(
        "{}|{}|{}|{}|{}",
        e.key, e.total, e.cut, e.deposit, e.pending
    )
}

fn gold_fingerprint(g: &GoldPoint) -> String {
    format!("gold|{}|{}", g.character, g.at)
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
/// silent no-op.
pub async fn upload_once(
    client: &reqwest::Client,
    token: &str,
    wow_retail_path: &Path,
    state_path: &Path,
    logger: &crate::logging::Logger,
) {
    if token.trim().is_empty() {
        return;
    }

    let mut state = UploadState::load_from(state_path);
    let mut sent = 0usize;
    let mut failed = 0usize;

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
                continue;
            }
        };

        let (entries, gold) = pending(&data, &state);
        if entries.is_empty() && gold.is_empty() {
            continue;
        }

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
                    break; // retry this file's remainder next tick
                }
            }
        }
    }

    if sent > 0 {
        if let Err(e) = state.save_to(state_path) {
            // The rows did land; only our note of it failed. Next tick re-sends
            // them and the server absorbs the duplicates.
            logger.error(&format!("could not persist upload state: {e}"));
        }
        logger.info(&format!("uploaded {sent} ledger rows"));
    }
    if failed > 0 {
        logger.error(&format!("{failed} ledger batches deferred to the next tick"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::savedvars::{parse_saved_variables, GoldPoint, LedgerData, LedgerEntry};

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

    #[test]
    fn everything_is_pending_on_a_first_run() {
        let data = LedgerData {
            entries: vec![entry("a"), entry("b")],
            gold: vec![gold(1)],
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
        };
        let (entries, _) = pending(&data, &state);
        assert_eq!(entries.len(), 1, "a row whose contents changed must be re-sent");
    }

    #[test]
    fn gold_points_dedupe_on_character_and_timestamp() {
        let mut state = UploadState::default();
        state.remember_gold(&[&gold(1)]);
        let data = LedgerData {
            entries: vec![],
            gold: vec![gold(1), gold(2)],
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
        };
        let (entries, _) = pending(&data, &UploadState::default());
        let batches: Vec<_> = entries.chunks(MAX_BATCH).collect();
        assert!(batches.iter().all(|b| b.len() <= MAX_BATCH));
        assert_eq!(batches.iter().map(|b| b.len()).sum::<usize>(), 1200);
    }
}
