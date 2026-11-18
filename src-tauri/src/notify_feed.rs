//! Notifications from goldcap.gg on the desktop. While paired, the Companion asks
//! `GET /v1/companion/notifications?since=<last id shown>` once a minute and shows each new one as
//! an OS notification. The site picks what the player asked to get on the Companion; this module
//! only shows each one once.
//!
//! The first poll with no saved id (a fresh install, or the first run of this version) shows
//! nothing: it only learns the newest id, and what arrives after that is shown.
//!
//! Each request carries the shared client's User-Agent, `goldcap-companion/<version>`: that is how
//! the site tells a Companion that shows notifications from one that does not.
//!
//! The last id shown lives in its own file (`notify-feed.json` in the config dir), not in the
//! config: Settings saves the whole config from the window, and every config change restarts the
//! sync loop.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

pub const STATE_FILE_NAME: &str = "notify-feed.json";
const POLL_EVERY: Duration = Duration::from_secs(60);
/// How many pages the first poll reads to find the newest id (the site sends 5 a page, from the
/// last two hours only).
const MAX_PRIME_PAGES: usize = 40;
/// The site reads `since` as at most 15 digits.
const MAX_SINCE: u64 = 999_999_999_999_999;
/// Only a page of the site opens from a click (Windows; the other systems report no click).
#[cfg_attr(not(windows), allow(dead_code))]
const SITE: &str = "https://goldcap.gg/";

/// One notification as the site sends it (`companionNotificationSchema`).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeedItem {
    pub id: String,
    pub title: String,
    pub body: String,
    pub url: String,
    /// The player's quiet hours cover now: shown without a sound.
    #[serde(default)]
    pub silent: bool,
}

#[derive(Deserialize)]
struct FeedBody {
    items: Vec<FeedItem>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct FeedState {
    /// The highest notification id shown on this computer.
    pub last_id: u64,
}

impl FeedState {
    #[cfg(test)]
    pub fn load_from(path: &Path) -> Self {
        Self::load_saved(path).unwrap_or_default()
    }

    /// None when there is no readable file: a fresh install, or the first run of this version.
    pub fn load_saved(path: &Path) -> Option<Self> {
        std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str(&t).ok())
    }

    pub fn save_to(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(self).map_err(std::io::Error::from)?;
        crate::luafile::write_atomic(path, &text)
    }
}

#[derive(Debug, PartialEq)]
pub enum Fetched {
    Items(Vec<FeedItem>),
    Unauthorized,
    Failed(String),
}

pub async fn fetch(client: &reqwest::Client, base: &str, token: &str, since: u64) -> Fetched {
    let sent = client
        .get(format!("{base}/v1/companion/notifications?since={}", since.min(MAX_SINCE)))
        .bearer_auth(token)
        .send()
        .await;
    match sent {
        Err(e) => Fetched::Failed(crate::sync::describe_err(&e, crate::sync::CLIENT_TIMEOUT)),
        Ok(res) => match res.status().as_u16() {
            200 => match res.json::<FeedBody>().await {
                Ok(body) => Fetched::Items(body.items),
                Err(e) => Fetched::Failed(format!("unreadable answer: {e}")),
            },
            401 => Fetched::Unauthorized,
            code => Fetched::Failed(format!("status {code}")),
        },
    }
}

/// What to show out of an answer, oldest first, and the id to ask after next time: only ids above
/// `last`, each once, and never one whose id is not a whole number.
pub fn fresh(items: Vec<FeedItem>, last: u64) -> (Vec<FeedItem>, u64) {
    let mut by_id: BTreeMap<u64, FeedItem> = BTreeMap::new();
    for item in items {
        let Ok(id) = item.id.trim().parse::<u64>() else { continue };
        if id > last {
            by_id.entry(id).or_insert(item);
        }
    }
    let next = by_id.keys().next_back().copied().unwrap_or(last).max(last);
    (by_id.into_values().collect(), next)
}

/// The poll's memory for the life of the process.
#[derive(Debug, Default)]
pub struct Feed {
    pub last_id: u64,
    /// A last id is known (saved, or found by a first poll). Until then a poll shows nothing: it
    /// only learns the newest id, so a fresh install never shows a burst of old notifications.
    primed: bool,
    /// The token the site refused: not asked again until the pairing changes.
    refused: Option<String>,
    /// The last failure logged, so a site that is down is one log line, not one a minute.
    failing: Option<String>,
}

impl Feed {
    pub fn load(state_path: &Path) -> Self {
        match FeedState::load_saved(state_path) {
            Some(saved) => Feed { last_id: saved.last_id, primed: true, ..Feed::default() },
            None => Feed::default(),
        }
    }

    /// One poll: what to show now. Silent (no request) while unpaired or after the site refused
    /// this token. The new last id is written before anything is shown, so a crash costs a
    /// notification rather than showing one twice; it is kept in memory even when the write fails.
    pub async fn poll(
        &mut self,
        client: &reqwest::Client,
        base: &str,
        token: &str,
        state_path: &Path,
        logger: &crate::logging::Logger,
    ) -> Vec<FeedItem> {
        let token = token.trim();
        if token.is_empty() || self.refused.as_deref() == Some(token) {
            return Vec::new();
        }
        // The first poll pages through what the site still holds and shows none of it.
        let pages = if self.primed { 1 } else { MAX_PRIME_PAGES };
        for _ in 0..pages {
            let items = match fetch(client, base, token, self.last_id).await {
                Fetched::Items(items) => items,
                other => return self.failed(other, token, logger),
            };
            if self.failing.take().is_some() {
                logger.info("notifications: reachable again");
            }
            let (show, next) = fresh(items, self.last_id);
            let moved = next != self.last_id;
            if moved || !self.primed {
                self.last_id = next;
                if let Err(e) = (FeedState { last_id: next }).save_to(state_path) {
                    logger.error(&format!("notifications: last shown id not saved ({e})"));
                }
            }
            if self.primed {
                return show;
            }
            if !moved {
                self.primed = true;
                logger.info(&format!("notifications: starting after id {}", self.last_id));
                return Vec::new();
            }
        }
        // Still more pages than expected: carry on reading silently next minute.
        Vec::new()
    }

    fn failed(&mut self, fetched: Fetched, token: &str, logger: &crate::logging::Logger) -> Vec<FeedItem> {
        match fetched {
            Fetched::Items(_) => Vec::new(),
            Fetched::Unauthorized => {
                logger.error("notifications: token refused (401), not asked again until the Companion is paired again");
                self.refused = Some(token.to_string());
                Vec::new()
            }
            Fetched::Failed(why) => {
                if self.failing.as_deref() != Some(why.as_str()) {
                    logger.error(&format!("notifications: will ask again ({why})"));
                    self.failing = Some(why);
                }
                Vec::new()
            }
        }
    }
}

/// The OS sound for a notification: none in quiet hours. On Windows a toast without one is silent,
/// and macOS plays one only when named.
pub fn sound_for(silent: bool) -> Option<&'static str> {
    if silent {
        return None;
    }
    if cfg!(target_os = "macos") {
        Some("NSUserNotificationDefaultSoundName")
    } else if cfg!(windows) {
        Some("Default")
    } else {
        Some("message-new-instant")
    }
}

/// Opens only a page of the site.
#[cfg_attr(not(windows), allow(dead_code))]
fn opens(url: &str) -> bool {
    url.starts_with(SITE)
}

/// Shows one notification. On Windows a click on it opens its page on goldcap.gg; macOS and Linux
/// show it without that.
fn show(app: &tauri::AppHandle, item: &FeedItem, logger: &crate::logging::Logger) {
    #[cfg(windows)]
    {
        if let Err(e) = show_windows(app, item) {
            logger.error(&format!("notification not shown: {e}"));
        }
    }
    #[cfg(not(windows))]
    {
        use tauri_plugin_notification::NotificationExt;
        let mut builder = app.notification().builder().title(&item.title).body(&item.body);
        if let Some(sound) = sound_for(item.silent) {
            builder = builder.sound(sound);
        }
        // Best effort: the player can turn notifications off for the app in the OS.
        if let Err(e) = builder.show() {
            logger.error(&format!("notification not shown: {e}"));
        }
    }
}

#[cfg(windows)]
fn show_windows(app: &tauri::AppHandle, item: &FeedItem) -> Result<(), String> {
    let mut toast = notify_rust::Notification::new();
    toast.summary(&item.title).body(&item.body);
    if let Some(sound) = sound_for(item.silent) {
        toast.sound_name(sound);
    }
    // The installed app's own id, as tauri-plugin-notification sets it; a build run from `target`
    // has none registered and shows under the default one.
    let installed = tauri::utils::platform::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|d| d.display().to_string()))
        .is_some_and(|dir| !dir.ends_with("\\target\\debug") && !dir.ends_with("\\target\\release"));
    if installed {
        toast.app_id(&app.config().identifier);
    }
    let handle = toast.show().map_err(|e| e.to_string())?;
    let app = app.clone();
    let url = item.url.clone();
    // Waits until the toast is clicked or leaves the screen (Windows reports both).
    std::thread::spawn(move || {
        let _ = handle.wait_for_response(move |response: &notify_rust::NotificationResponse| {
            if matches!(response, notify_rust::NotificationResponse::Default) && opens(&url) {
                let _ = crate::commands::open_in_browser(&app, &url);
            }
        });
    });
    Ok(())
}

/// The loop: once a minute, while the app runs. The token is read from the current config each
/// time, so pairing and unpairing take effect on the next poll.
pub async fn run_loop(
    app: tauri::AppHandle,
    client: reqwest::Client,
    config_rx: tokio::sync::watch::Receiver<crate::config::Config>,
    state_path: std::path::PathBuf,
    logger: std::sync::Arc<crate::logging::Logger>,
) {
    let mut feed = Feed::load(&state_path);
    let mut ticker = tokio::time::interval(POLL_EVERY);
    // After sleep, one poll, not a burst of the missed ones.
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        let token = config_rx.borrow().companion_token.clone();
        for item in feed.poll(&client, crate::forever::API_BASE, &token, &state_path, &logger).await {
            show(&app, &item, &logger);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forever::tests::{answer, serve_forever, serve_once};

    fn item(id: &str) -> FeedItem {
        FeedItem {
            id: id.into(),
            title: format!("Deal {id}"),
            body: "Linen Cloth at 70c".into(),
            url: "https://goldcap.gg/items/2589".into(),
            silent: false,
        }
    }

    fn ids(items: &[FeedItem]) -> Vec<&str> {
        items.iter().map(|i| i.id.as_str()).collect()
    }

    #[test]
    fn only_ids_above_the_last_one_shown_each_once_oldest_first() {
        let (show, next) = fresh(vec![item("12"), item("10"), item("11"), item("12"), item("9")], 10);
        assert_eq!(ids(&show), vec!["11", "12"]);
        assert_eq!(next, 12);

        let (show, next) = fresh(vec![item("12"), item("11")], 12);
        assert!(show.is_empty());
        assert_eq!(next, 12, "the last id never goes back");

        let (show, next) = fresh(vec![item("abc"), item("-3"), item(""), item("13")], 12);
        assert_eq!(ids(&show), vec!["13"], "an id that is not a whole number is never shown");
        assert_eq!(next, 13);

        let (show, next) = fresh(Vec::new(), 7);
        assert!(show.is_empty());
        assert_eq!(next, 7);
    }

    #[test]
    fn quiet_hours_make_no_sound() {
        assert_eq!(sound_for(true), None);
        assert!(sound_for(false).is_some());
    }

    #[test]
    fn a_click_opens_only_a_page_of_the_site() {
        assert!(opens("https://goldcap.gg/items/2589"));
        assert!(!opens("https://goldcap.gg.evil.example/"));
        assert!(!opens("http://goldcap.gg/items/2589"));
        assert!(!opens("file:///etc/passwd"));
    }

    fn setup(name: &str) -> (std::path::PathBuf, std::path::PathBuf, crate::logging::Logger) {
        let dir = std::env::temp_dir().join(format!("goldcap-notify-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let logger = crate::logging::Logger::new(&dir.join("logs")).unwrap();
        (dir.clone(), dir.join(STATE_FILE_NAME), logger)
    }

    fn feed_of(items: &[(&str, bool)]) -> String {
        let items: Vec<serde_json::Value> = items
            .iter()
            .map(|(id, silent)| serde_json::json!({ "id": id, "title": "t", "body": "b", "url": "https://goldcap.gg/", "iconUrl": null, "silent": silent }))
            .collect();
        serde_json::json!({ "items": items }).to_string()
    }

    #[tokio::test]
    async fn the_poll_asks_since_the_last_shown_and_never_shows_one_twice() {
        let (dir, state_path, logger) = setup("since");
        let client = crate::sync::build_client();
        FeedState { last_id: 40 }.save_to(&state_path).unwrap();
        let mut feed = Feed::load(&state_path);
        assert_eq!(feed.last_id, 40);

        let (base, seen) = serve_once(answer("200 OK", &feed_of(&[("41", false), ("42", true)])));
        let shown = feed.poll(&client, &base, "tok", &state_path, &logger).await;
        assert_eq!(ids(&shown), vec!["41", "42"]);
        assert!(shown[1].silent);
        let request = seen.recv().unwrap();
        let lower = request.to_ascii_lowercase();
        assert!(lower.starts_with("get /v1/companion/notifications?since=40 "), "{request}");
        assert!(lower.contains("authorization: bearer tok"));
        assert!(lower.contains(&format!("user-agent: goldcap-companion/{}", env!("CARGO_PKG_VERSION"))), "{request}");
        assert_eq!(FeedState::load_from(&state_path).last_id, 42, "kept on disk before anything is shown");

        // The site answers 42 again (a response it never knew arrived): asked since 42, not shown.
        let (base, seen) = serve_once(answer("200 OK", &feed_of(&[("42", false), ("43", false)])));
        let shown = feed.poll(&client, &base, "tok", &state_path, &logger).await;
        assert_eq!(ids(&shown), vec!["43"]);
        assert!(seen.recv().unwrap().starts_with("GET /v1/companion/notifications?since=42 "));

        // A restart reads the last id back.
        let mut again = Feed::load(&state_path);
        assert_eq!(again.last_id, 43);
        let (base, seen) = serve_once(answer("200 OK", &feed_of(&[("43", false)])));
        assert!(again.poll(&client, &base, "tok", &state_path, &logger).await.is_empty());
        assert!(seen.recv().unwrap().starts_with("GET /v1/companion/notifications?since=43 "));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn unpaired_or_refused_it_stays_silent() {
        let (dir, state_path, logger) = setup("silent");
        let client = crate::sync::build_client();
        FeedState { last_id: 0 }.save_to(&state_path).unwrap();
        let mut feed = Feed::load(&state_path);

        // No token: no request (nothing listens on port 9, and nothing is shown).
        assert!(feed.poll(&client, "http://127.0.0.1:9", "  ", &state_path, &logger).await.is_empty());

        // A refused token is not asked again; a new pairing is.
        let (base, _seen) = serve_once(answer("401 Unauthorized", r#"{"error":"unauthorized"}"#));
        assert!(feed.poll(&client, &base, "old", &state_path, &logger).await.is_empty());
        assert!(feed.poll(&client, "http://127.0.0.1:9", "old", &state_path, &logger).await.is_empty());
        let (base, seen) = serve_once(answer("200 OK", &feed_of(&[("5", false)])));
        assert_eq!(ids(&feed.poll(&client, &base, "new", &state_path, &logger).await), vec!["5"]);
        assert!(seen.recv().unwrap().to_ascii_lowercase().contains("authorization: bearer new"));

        // A failing site shows nothing and keeps the last id.
        let (base, _seen) = serve_once(answer("500 Internal Server Error", "{}"));
        assert!(feed.poll(&client, &base, "new", &state_path, &logger).await.is_empty());
        let (base, _seen) = serve_once(answer("200 OK", "not json"));
        assert!(feed.poll(&client, &base, "new", &state_path, &logger).await.is_empty());
        assert_eq!(feed.last_id, 5);
        assert_eq!(FeedState::load_from(&state_path).last_id, 5);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn the_first_poll_shows_nothing_and_starts_after_the_newest_id() {
        let (dir, state_path, logger) = setup("prime");
        let client = crate::sync::build_client();
        let mut feed = Feed::load(&state_path);

        // A fresh install: the site still holds seven, five a page. All are read, none shown.
        let (base, seen) = serve_forever(vec![
            answer("200 OK", &feed_of(&[("1", false), ("2", false), ("3", false), ("4", false), ("5", false)])),
            answer("200 OK", &feed_of(&[("6", false), ("7", false)])),
            answer("200 OK", &feed_of(&[])),
        ]);
        assert!(feed.poll(&client, &base, "tok", &state_path, &logger).await.is_empty());
        let asked: Vec<String> = seen.try_iter().map(|r| r.lines().next().unwrap().to_string()).collect();
        assert_eq!(
            asked,
            vec![
                "GET /v1/companion/notifications?since=0 HTTP/1.1",
                "GET /v1/companion/notifications?since=5 HTTP/1.1",
                "GET /v1/companion/notifications?since=7 HTTP/1.1",
            ]
        );
        assert_eq!(feed.last_id, 7);
        assert_eq!(FeedState::load_from(&state_path).last_id, 7);

        // Only what arrives after that is shown.
        let (base, _seen) = serve_once(answer("200 OK", &feed_of(&[("8", false)])));
        assert_eq!(ids(&feed.poll(&client, &base, "tok", &state_path, &logger).await), vec!["8"]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn a_failed_first_poll_stays_silent_until_one_succeeds_and_a_broken_file_starts_over() {
        let (dir, state_path, logger) = setup("prime-fail");
        let client = crate::sync::build_client();
        std::fs::write(&state_path, "{ broken").unwrap();
        let mut feed = Feed::load(&state_path);

        let (base, _seen) = serve_once(answer("500 Internal Server Error", "{}"));
        assert!(feed.poll(&client, &base, "tok", &state_path, &logger).await.is_empty());

        // The site holds nothing: the first poll still counts, and saves where it starts.
        let (base, _seen) = serve_once(answer("200 OK", &feed_of(&[])));
        assert!(feed.poll(&client, &base, "tok", &state_path, &logger).await.is_empty());
        assert_eq!(FeedState::load_saved(&state_path), Some(FeedState { last_id: 0 }));

        let (base, _seen) = serve_once(answer("200 OK", &feed_of(&[("3", false)])));
        assert_eq!(ids(&feed.poll(&client, &base, "tok", &state_path, &logger).await), vec!["3"]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_missing_or_broken_state_file_starts_at_zero() {
        let (dir, state_path, _logger) = setup("state");
        assert_eq!(FeedState::load_from(&state_path), FeedState::default());
        std::fs::write(&state_path, "{ broken").unwrap();
        assert_eq!(FeedState::load_from(&state_path).last_id, 0);
        FeedState { last_id: 9 }.save_to(&state_path).unwrap();
        assert_eq!(std::fs::read_to_string(&state_path).unwrap(), "{\n  \"lastId\": 9\n}");
        std::fs::remove_dir_all(&dir).ok();
    }
}
