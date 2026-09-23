//! The whole-market region payload (GCM1): every commodity of the region with its
//! market facts, fetched from `GET /v1/addon/region-data` after each import-string
//! fetch and written into `AppData.lua` beside the import string as `regionString`.
//! The last good body is kept in memory for the life of the process, so an unchanged
//! payload costs a 304 and a failure costs a log line -- the import string is
//! written either way.

use crate::logging::Logger;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

pub const REGION_DATA_URL: &str = "https://api.goldcap.gg/v1/addon/region-data";

/// A body larger than this is never kept. The site refuses to build one over
/// 4,000,000 characters and the addon to parse one over 6,000,000, so anything this
/// big is not a payload either side would use.
pub const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;

/// Older than this, by the payload's own snapshot time, and the addon would call every
/// fact in it stale (its SOURCE_MAX_AGE is the same three hours) -- so it is not written.
pub const MAX_AGE_SECS: i64 = 3 * 3600;

/// The payload is ~1.2 MB and built on the site's first request after an ingest tick,
/// so it gets three times the import string's 20 s.
const TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RegionSummary {
    pub items: u32,
    pub ts: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct KeptPayload {
    pub region: String,
    pub body: String,
    pub etag: Option<String>,
    pub summary: RegionSummary,
}

#[derive(Debug, PartialEq)]
pub enum Fetched {
    Fresh { body: String, etag: Option<String> },
    NotModified,
}

/// Header check and item count: `GCM1;<region>;<ts>;` with at least one `I` token and
/// at most MAX_BODY_BYTES. Anything else -- another region, a buy-run string, an error
/// page -- is never kept.
pub fn summarize(body: &str, region: &str) -> Result<RegionSummary, String> {
    if body.len() > MAX_BODY_BYTES {
        return Err(format!("region data too large ({} bytes)", body.len()));
    }
    let prefix = format!("GCM1;{region};");
    let rest = body
        .strip_prefix(&prefix)
        .ok_or_else(|| format!("response was not GCM1 region data for {region}"))?;
    let (ts, sections) = rest
        .split_once(';')
        .ok_or_else(|| "region data has no sections".to_string())?;
    let ts: i64 = ts
        .parse()
        .map_err(|_| "region data has no snapshot time".to_string())?;
    let items = sections
        .split(';')
        .find_map(|section| section.strip_prefix("I:"))
        .map(|tokens| {
            if tokens.is_empty() {
                0
            } else {
                tokens.split(',').count() as u32
            }
        })
        .unwrap_or(0);
    if items == 0 {
        return Err("region data carried no items".to_string());
    }
    Ok(RegionSummary { items, ts })
}

/// What this tick leaves kept, given what was kept before and what the fetch returned.
/// Pure: every rule about keeping and dropping lives here, so it is tested without a
/// network. A payload kept for another region is dropped -- the setting changed under it.
pub fn settle(
    kept: Option<KeptPayload>,
    fetched: Result<Fetched, String>,
    region: &str,
) -> (Option<KeptPayload>, Result<RegionSummary, String>) {
    let kept = kept.filter(|k| k.region == region);
    match fetched {
        Ok(Fetched::Fresh { body, etag }) => match summarize(&body, region) {
            Ok(summary) => (
                Some(KeptPayload {
                    region: region.to_string(),
                    body,
                    etag,
                    summary,
                }),
                Ok(summary),
            ),
            Err(e) => (kept, Err(e)),
        },
        Ok(Fetched::NotModified) => match kept {
            Some(k) => {
                let summary = k.summary;
                (Some(k), Ok(summary))
            }
            None => (None, Err("304 with no region data kept".to_string())),
        },
        Err(e) => (kept, Err(e)),
    }
}

/// The ETag to revalidate with -- only while a body is kept for this region. A 304 to a
/// request with nothing kept would leave nothing to write.
pub fn if_none_match<'a>(kept: Option<&'a KeptPayload>, region: &str) -> Option<&'a str> {
    kept.filter(|k| k.region == region)
        .and_then(|k| k.etag.as_deref())
}

/// The body to write into `AppData.lua` this tick: the kept one for this region, unless
/// its snapshot is older than MAX_AGE_SECS.
pub fn to_write<'a>(kept: Option<&'a KeptPayload>, region: &str, now: i64) -> Option<&'a str> {
    kept.filter(|k| k.region == region && now - k.summary.ts <= MAX_AGE_SECS)
        .map(|k| k.body.as_str())
}

async fn fetch_from(
    client: &reqwest::Client,
    url: &str,
    region: &str,
    etag: Option<&str>,
) -> Result<Fetched, String> {
    let mut request = client
        .get(url)
        .query(&[("region", region)])
        .timeout(TIMEOUT);
    if let Some(tag) = etag {
        request = request.header(reqwest::header::IF_NONE_MATCH, tag);
    }
    let mut resp = request.send().await.map_err(|e| e.to_string())?;
    match resp.status().as_u16() {
        304 => return Ok(Fetched::NotModified),
        200 => {}
        code => return Err(format!("unexpected status {code}")),
    }
    if resp
        .content_length()
        .is_some_and(|n| n > MAX_BODY_BYTES as u64)
    {
        return Err("region data too large".to_string());
    }
    let etag = resp
        .headers()
        .get(reqwest::header::ETAG)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    // Read under a running cap, not with `text()`: a hop that answers chunked declares no
    // length for the check above, and `text()` would buffer whatever it sent for 60 s.
    let mut bytes = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(|e| e.to_string())? {
        if bytes.len() + chunk.len() > MAX_BODY_BYTES {
            return Err("region data too large".to_string());
        }
        bytes.extend_from_slice(&chunk);
    }
    // What `text()` does without reqwest's `charset` feature, which this build leaves off.
    let body = String::from_utf8_lossy(&bytes).into_owned();
    Ok(Fetched::Fresh { body, etag })
}

/// The last good payload, for the life of the process.
fn kept_store() -> &'static Mutex<Option<KeptPayload>> {
    static KEPT: OnceLock<Mutex<Option<KeptPayload>>> = OnceLock::new();
    KEPT.get_or_init(|| Mutex::new(None))
}

/// One tick's region leg: revalidate or fetch, settle, log, and hand back the body to write
/// with its summary for the Status screen -- one pair, so the two cannot disagree. Never an
/// error: a failure is a log line and the import string is written whatever this returns.
pub async fn refresh(
    client: &reqwest::Client,
    region: &str,
    logger: &Logger,
    now: i64,
) -> Option<(String, RegionSummary)> {
    refresh_at(client, REGION_DATA_URL, kept_store(), region, logger, now).await
}

/// `refresh` against a given URL and store, so whole ticks run in tests against a local
/// server without touching the process's own kept payload. The ETag comes from what was
/// kept *before* this tick, and the store is always put back.
async fn refresh_at(
    client: &reqwest::Client,
    url: &str,
    store: &Mutex<Option<KeptPayload>>,
    region: &str,
    logger: &Logger,
    now: i64,
) -> Option<(String, RegionSummary)> {
    let previous = store.lock().unwrap_or_else(|p| p.into_inner()).take();
    let etag = if_none_match(previous.as_ref(), region).map(str::to_string);
    let fetched = fetch_from(client, url, region, etag.as_deref()).await;
    let unchanged = matches!(fetched, Ok(Fetched::NotModified));
    let (kept, outcome) = settle(previous, fetched, region);
    match outcome {
        Ok(summary) => logger.info(&format!(
            "region data {region}: {} {}, {} min old{}",
            summary.items,
            if summary.items == 1 { "item" } else { "items" },
            (now - summary.ts).max(0) / 60,
            if unchanged { " (unchanged)" } else { "" }
        )),
        Err(e) => logger.error(&format!("region data {region} failed: {e}")),
    }
    let written = kept
        .as_ref()
        .and_then(|k| to_write(Some(k), region, now).map(|body| (body.to_string(), k.summary)));
    if written.is_none() && kept.is_some() {
        logger.info(&format!(
            "region data {region}: older than 3 h, not written"
        ));
    }
    *store.lock().unwrap_or_else(|p| p.into_inner()) = kept;
    written
}

#[cfg(test)]
mod tests {
    use super::*;

    const BODY: &str =
        "GCM1;eu;1789819200;I:1=2,3=4=5.0;V:1=1789819200=1=1=1=1=1=1=1=0;M:9=20000=2";

    fn kept(region: &str, ts: i64) -> KeptPayload {
        KeptPayload {
            region: region.to_string(),
            body: format!("GCM1;{region};{ts};I:1=2"),
            etag: Some(format!("\"gcm1-{region}-{ts}\"")),
            summary: RegionSummary { items: 1, ts },
        }
    }

    #[test]
    fn summarize_counts_the_i_section_and_reads_the_snapshot_time() {
        assert_eq!(
            summarize(BODY, "eu"),
            Ok(RegionSummary {
                items: 2,
                ts: 1_789_819_200
            })
        );
    }

    #[test]
    fn summarize_refuses_another_region_a_run_string_and_an_import_string() {
        assert!(summarize(BODY, "us").is_err());
        assert!(summarize("GCR1;abcd2345;Cooking;5=210", "eu").is_err());
        assert!(summarize("GCS1;eu;dentarg;1;I:1=2", "eu").is_err());
        assert!(summarize("no_data", "eu").is_err());
    }

    #[test]
    fn summarize_refuses_a_payload_without_items_or_a_timestamp() {
        assert!(summarize("GCM1;eu;1;I:", "eu").is_err());
        assert!(summarize("GCM1;eu;1;M:9=20000=2", "eu").is_err());
        assert!(summarize("GCM1;eu;soon;I:1=2", "eu").is_err());
    }

    #[test]
    fn summarize_refuses_a_body_over_eight_megabytes() {
        let big = format!("GCM1;eu;1;I:1=2;M:{}", "9".repeat(MAX_BODY_BYTES));
        assert!(summarize(&big, "eu").is_err());
    }

    #[test]
    fn a_fresh_body_replaces_what_was_kept() {
        let (now_kept, outcome) = settle(
            Some(kept("eu", 1)),
            Ok(Fetched::Fresh {
                body: BODY.to_string(),
                etag: Some("\"gcm1-eu-1789819200\"".into()),
            }),
            "eu",
        );
        let now_kept = now_kept.unwrap();
        assert_eq!(now_kept.body, BODY);
        assert_eq!(now_kept.etag.as_deref(), Some("\"gcm1-eu-1789819200\""));
        assert_eq!(
            outcome,
            Ok(RegionSummary {
                items: 2,
                ts: 1_789_819_200
            })
        );
    }

    #[test]
    fn a_304_reuses_the_kept_body() {
        let before = kept("eu", 1_789_819_200);
        let (after, outcome) = settle(Some(before.clone()), Ok(Fetched::NotModified), "eu");
        assert_eq!(after, Some(before));
        assert_eq!(
            outcome,
            Ok(RegionSummary {
                items: 1,
                ts: 1_789_819_200
            })
        );
    }

    #[test]
    fn a_304_with_nothing_kept_is_a_failure() {
        let (after, outcome) = settle(None, Ok(Fetched::NotModified), "eu");
        assert_eq!(after, None);
        assert!(outcome.is_err());
    }

    #[test]
    fn a_failed_or_refused_fetch_keeps_the_last_good_body() {
        let before = kept("eu", 1_789_819_200);
        let (after, outcome) = settle(Some(before.clone()), Err("timed out".into()), "eu");
        assert_eq!(after, Some(before.clone()));
        assert_eq!(outcome, Err("timed out".to_string()));

        let (after, outcome) = settle(
            Some(before.clone()),
            Ok(Fetched::Fresh {
                body: "<html>502</html>".into(),
                etag: None,
            }),
            "eu",
        );
        assert_eq!(after, Some(before));
        assert!(outcome.is_err());
    }

    #[test]
    fn a_kept_payload_from_another_region_is_neither_revalidated_nor_written() {
        let eu = kept("eu", 1_789_819_200);
        assert_eq!(if_none_match(Some(&eu), "us"), None);
        assert_eq!(to_write(Some(&eu), "us", 1_789_819_200), None);
        let (after, outcome) = settle(Some(eu), Ok(Fetched::NotModified), "us");
        assert_eq!(after, None);
        assert!(outcome.is_err());
    }

    #[test]
    fn the_etag_is_sent_only_while_a_body_is_kept() {
        let eu = kept("eu", 1);
        assert_eq!(if_none_match(Some(&eu), "eu"), Some("\"gcm1-eu-1\""));
        assert_eq!(if_none_match(None, "eu"), None);
    }

    #[test]
    fn a_payload_older_than_three_hours_is_not_written() {
        let eu = kept("eu", 1_000_000);
        assert_eq!(
            to_write(Some(&eu), "eu", 1_000_000 + MAX_AGE_SECS),
            Some(eu.body.as_str())
        );
        assert_eq!(
            to_write(Some(&eu), "eu", 1_000_000 + MAX_AGE_SECS + 1),
            None
        );
    }

    /// One canned HTTP answer on a local port; hands back the URL and the raw request it saw.
    fn serve_once(response: String) -> (String, std::sync::mpsc::Receiver<String>) {
        serve_and_hold(response, Duration::ZERO)
    }

    /// `serve_once`, but the connection stays open for `hold` after the answer is sent, so
    /// an answer without its end keeps a reader that wants the end waiting.
    fn serve_and_hold(
        response: String,
        hold: Duration,
    ) -> (String, std::sync::mpsc::Receiver<String>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!(
            "http://{}/v1/addon/region-data",
            listener.local_addr().unwrap()
        );
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let mut buf = [0u8; 4096];
            while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                let n = stream.read(&mut buf).unwrap();
                if n == 0 {
                    break;
                }
                request.extend_from_slice(&buf[..n]);
            }
            tx.send(String::from_utf8_lossy(&request).into_owned())
                .unwrap();
            // The client may hang up mid-answer (that is what a refused body looks like).
            let _ = stream.write_all(response.as_bytes());
            std::thread::sleep(hold);
        });
        (url, rx)
    }

    /// A 200 with no Content-Length whose body comes in HTTP/1.1 chunks, the way a hop
    /// that re-encodes answers.
    fn chunked_answer(parts: &[&str]) -> String {
        chunked_unfinished(parts) + "0\r\n\r\n"
    }

    /// `chunked_answer` without the final zero-length chunk: as far as the reader can tell,
    /// more body is on its way.
    fn chunked_unfinished(parts: &[&str]) -> String {
        let mut out = String::from(
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
        );
        for part in parts {
            out.push_str(&format!("{:x}\r\n{part}\r\n", part.len()));
        }
        out
    }

    #[tokio::test]
    async fn a_revalidation_sends_the_kept_etag_and_reads_304_as_unchanged() {
        let (url, seen) = serve_once(
            "HTTP/1.1 304 Not Modified\r\nETag: \"gcm1-eu-1\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                .to_string(),
        );
        let fetched = fetch_from(
            &crate::sync::build_client(),
            &url,
            "eu",
            Some("\"gcm1-eu-1\""),
        )
        .await;
        assert_eq!(fetched, Ok(Fetched::NotModified));
        let request = seen.recv().unwrap().to_ascii_lowercase();
        assert!(
            request.contains("if-none-match: \"gcm1-eu-1\""),
            "{request}"
        );
        assert!(request.contains("region=eu"), "{request}");
    }

    #[tokio::test]
    async fn a_fresh_answer_brings_its_body_and_etag() {
        let (url, _seen) = serve_once(format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nETag: \"gcm1-eu-1789819200\"\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{BODY}",
            BODY.len()
        ));
        let fetched = fetch_from(&crate::sync::build_client(), &url, "eu", None).await;
        assert_eq!(
            fetched,
            Ok(Fetched::Fresh {
                body: BODY.to_string(),
                etag: Some("\"gcm1-eu-1789819200\"".to_string()),
            })
        );
    }

    #[tokio::test]
    async fn another_status_is_a_failure() {
        let (url, _seen) = serve_once(
            "HTTP/1.1 404 Not Found\r\nContent-Length: 7\r\nConnection: close\r\n\r\nno_data"
                .to_string(),
        );
        let fetched = fetch_from(&crate::sync::build_client(), &url, "eu", None).await;
        assert!(fetched.is_err());
    }

    fn test_logger(label: &str) -> (Logger, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "goldcap-companion-region-{label}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        (Logger::new(&dir).unwrap(), dir)
    }

    fn log_of(dir: &std::path::Path) -> String {
        std::fs::read_to_string(dir.join(crate::logging::LOG_FILE_NAME)).unwrap_or_default()
    }

    const NOT_MODIFIED: &str =
        "HTTP/1.1 304 Not Modified\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";

    #[tokio::test]
    async fn a_region_switch_between_ticks_neither_revalidates_nor_writes_the_old_payload() {
        let client = crate::sync::build_client();
        let store = Mutex::new(None);
        let (logger, dir) = test_logger("switch");
        let ts = 1_789_819_200;
        let now = ts + 600;
        let one = format!("GCM1;eu;{ts};I:1=2");

        // Tick 1, eu: a fresh one-item payload is kept and handed back to write.
        let (url, _seen) = serve_once(format!(
            "HTTP/1.1 200 OK\r\nETag: \"gcm1-eu-{ts}\"\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{one}",
            one.len()
        ));
        let first = refresh_at(&client, &url, &store, "eu", &logger, now).await;
        assert_eq!(first, Some((one.clone(), RegionSummary { items: 1, ts })));

        // Tick 2, eu: revalidated with the kept ETag, and the 304 hands back the same body.
        let (url, seen) = serve_once(NOT_MODIFIED.to_string());
        let second = refresh_at(&client, &url, &store, "eu", &logger, now).await;
        assert_eq!(second, first);
        let request = seen.recv().unwrap().to_ascii_lowercase();
        assert!(
            request.contains(&format!("if-none-match: \"gcm1-eu-{ts}\"")),
            "{request}"
        );

        // Tick 3, the setting now says us: the eu ETag stays home, and a 304 (a proxy's)
        // writes nothing -- neither the eu payload nor anything else.
        let (url, seen) = serve_once(NOT_MODIFIED.to_string());
        let third = refresh_at(&client, &url, &store, "us", &logger, now).await;
        assert_eq!(third, None);
        let request = seen.recv().unwrap().to_ascii_lowercase();
        assert!(!request.contains("if-none-match"), "{request}");
        assert!(request.contains("region=us"), "{request}");
        assert_eq!(*store.lock().unwrap(), None);

        let log = log_of(&dir);
        assert!(
            log.contains("region data eu: 1 item, 10 min old\n"),
            "{log}"
        );
        assert!(
            log.contains("region data eu: 1 item, 10 min old (unchanged)\n"),
            "{log}"
        );
        assert!(log.contains("region data us failed"), "{log}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn a_region_failure_still_lets_the_import_string_be_written() {
        let client = crate::sync::build_client();
        let store = Mutex::new(None);
        let (logger, dir) = test_logger("failure");
        let (url, _seen) = serve_once(
            "HTTP/1.1 502 Bad Gateway\r\nContent-Length: 16\r\nConnection: close\r\n\r\n<html>502</html>"
                .to_string(),
        );

        let leg = refresh_at(&client, &url, &store, "eu", &logger, 1_789_819_200).await;
        assert_eq!(leg, None);

        let wow = dir.join("wow");
        std::fs::create_dir_all(&wow).unwrap();
        let written = crate::sync::write_prices(&wow, "GCS1;eu;dentarg;1;abc", leg);
        assert_eq!(written.unwrap(), None);
        let lua = crate::luafile::addon_dir(&wow).join(crate::luafile::LUA_FILE_NAME);
        let contents = std::fs::read_to_string(lua).unwrap();
        assert!(
            contents.starts_with(
                "GoldCap_AppData = { importString = 'GCS1;eu;dentarg;1;abc', writtenAt = "
            ),
            "{contents}"
        );
        let log = log_of(&dir);
        assert!(
            log.contains("region data eu failed: unexpected status 502"),
            "{log}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn a_chunked_answer_is_read_whole() {
        let (head, rest) = BODY.split_at(10);
        let (middle, tail) = rest.split_at(20);
        let (url, _seen) = serve_once(chunked_answer(&[head, middle, tail]));
        let fetched = fetch_from(&crate::sync::build_client(), &url, "eu", None).await;
        assert_eq!(
            fetched,
            Ok(Fetched::Fresh {
                body: BODY.to_string(),
                etag: None,
            })
        );
    }

    #[tokio::test]
    async fn a_chunked_answer_over_eight_megabytes_is_refused() {
        // The cap plus one chunk, then the connection stays open with no final chunk. A
        // reader that stops at the cap answers at once; one that takes the whole body
        // before checking it is still waiting for the rest when the 5 s run out.
        let part = "9".repeat(64 * 1024);
        let parts = vec![part.as_str(); MAX_BODY_BYTES / part.len() + 1];
        let (url, _seen) = serve_and_hold(chunked_unfinished(&parts), Duration::from_secs(10));
        let fetched = tokio::time::timeout(
            Duration::from_secs(5),
            fetch_from(&crate::sync::build_client(), &url, "eu", None),
        )
        .await
        .expect("the body must be refused as it passes the cap, not once the peer is done");
        let error = fetched.expect_err("a body past the cap must not come back as Fresh");
        assert!(error.contains("too large"), "{error}");
    }
}
