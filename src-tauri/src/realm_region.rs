//! Which Retail region the player is on, from their own files: the `SET portal` line in
//! Config.wtf when there is one, else the region that knows the realm they played last. Never a
//! guess: with no portal and a realm name that several regions (or none) know, the answer is
//! "ask the player".
//!
//! Taiwanese realms log in through the Korean portal, so a KR (or TW) portal stands for both and
//! the realm name decides between them.

use crate::config::Region;
use std::path::Path;

const ALL: [Region; 4] = [Region::Us, Region::Eu, Region::Kr, Region::Tw];
const KR_PORTAL: [Region; 2] = [Region::Kr, Region::Tw];

/// The regions a portal value can mean: one for EU and US, KR and TW for either of those, and
/// all four for no portal (or one this build does not know, such as CN).
fn pool(portal: Option<&str>) -> &'static [Region] {
    match portal.and_then(|p| p.parse::<Region>().ok()) {
        Some(Region::Eu) => &[Region::Eu],
        Some(Region::Us) => &[Region::Us],
        Some(Region::Kr | Region::Tw) => &KR_PORTAL,
        None => &ALL,
    }
}

/// The region for a fresh setup. `known` is which of `pool(portal)` resolve the last-played
/// realm name; `None` when that could not be told (no answer from goldcap.gg).
pub fn decide(portal: Option<&str>, known: Option<&[Region]>) -> Option<Region> {
    let pool = pool(portal);
    let hits: Vec<Region> = pool.iter().copied().filter(|r| known.is_some_and(|k| k.contains(r))).collect();
    match hits[..] {
        [one] => Some(one),
        // A portal this build knows names its own region first (KR for the Korean portal).
        _ => (pool.len() < ALL.len()).then(|| pool[0]),
    }
}

/// The region an existing install should switch to, if any. `known` is which regions resolve
/// the last-played realm name, among those `hint_at` asked; `None` when it could not tell.
/// Only says something when the player's files disagree with `saved`: a portal for another
/// region, or a realm the saved region does not know and exactly one other region does.
pub fn hint(saved: Region, portal: Option<&str>, known: Option<&[Region]>) -> Option<Region> {
    let pool = pool(portal);
    if !pool.contains(&saved) {
        return decide(portal, known);
    }
    let known = known?;
    if known.contains(&saved) {
        return None;
    }
    let others: Vec<Region> = pool.iter().copied().filter(|r| *r != saved && known.contains(r)).collect();
    match others[..] {
        [one] => Some(one),
        _ => None,
    }
}

/// Which of `regions` know the realm `name`; `None` when any of them could not be asked.
async fn known_in(client: &reqwest::Client, base: &str, name: &str, regions: &[Region]) -> Option<Vec<Region>> {
    let mut known = Vec::new();
    for region in regions {
        if crate::sync::resolve_realm_at(client, base, &region.to_string(), name).await.ok()?.is_some() {
            known.push(*region);
        }
    }
    Some(known)
}

/// `hint` for a tick that already asked the saved region about `name` (`saved_known`: `None`
/// when that failed). Asks only the regions that can change the answer.
pub async fn hint_at(
    client: &reqwest::Client,
    base: &str,
    saved: Region,
    portal: Option<&str>,
    name: &str,
    saved_known: Option<&Option<String>>,
) -> Option<Region> {
    let pool = pool(portal);
    let probe: Vec<Region> = if pool.contains(&saved) {
        if !matches!(saved_known, Some(None)) {
            return None;
        }
        pool.iter().copied().filter(|r| *r != saved).collect()
    } else if pool.len() == 1 {
        Vec::new()
    } else {
        pool.to_vec()
    };
    let known = known_in(client, base, name, &probe).await;
    hint(saved, portal, known.as_deref())
}

/// The region for the wizard from a `_retail_` folder, or `None` for "ask the player".
pub async fn detect_at(client: &reqwest::Client, base: &str, wow_retail_path: &Path) -> Option<Region> {
    let game = crate::wtf::read_game_settings(wow_retail_path);
    let portal = game.region.as_deref();
    let pool = pool(portal);
    let known = match game.realm_names.first() {
        Some(name) if pool.len() > 1 => known_in(client, base, name, pool).await,
        _ => Some(Vec::new()),
    };
    decide(portal, known.as_deref())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use Region::*;

    #[test]
    fn a_portal_is_the_answer_and_kr_lets_the_realm_pick_tw() {
        assert_eq!(decide(Some("eu"), Some(&[])), Some(Eu));
        assert_eq!(decide(Some("us"), None), Some(Us));
        assert_eq!(decide(Some("kr"), Some(&[Kr])), Some(Kr));
        assert_eq!(decide(Some("kr"), Some(&[Tw])), Some(Tw));
        // Both or neither know it, or no answer: the portal's own region.
        assert_eq!(decide(Some("kr"), Some(&[Kr, Tw])), Some(Kr));
        assert_eq!(decide(Some("kr"), Some(&[])), Some(Kr));
        assert_eq!(decide(Some("kr"), None), Some(Kr));
        assert_eq!(decide(Some("tw"), Some(&[Tw])), Some(Tw));
    }

    #[test]
    fn no_portal_takes_the_one_region_that_knows_the_realm_and_otherwise_asks() {
        assert_eq!(decide(None, Some(&[Us])), Some(Us));
        assert_eq!(decide(None, Some(&[Tw])), Some(Tw));
        // Stormrage is on both US and EU: ask, never EU.
        assert_eq!(decide(None, Some(&[Us, Eu])), None);
        assert_eq!(decide(None, Some(&[])), None);
        assert_eq!(decide(None, None), None);
        // A portal this build has no region for counts as none.
        assert_eq!(decide(Some("cn"), Some(&[])), None);
    }

    #[test]
    fn an_existing_install_hears_about_another_region_only_on_evidence() {
        // The realm is not on the saved EU but is on exactly one other region.
        assert_eq!(hint(Eu, None, Some(&[Us])), Some(Us));
        // On the saved region, or on several others, or nowhere: nothing to say.
        assert_eq!(hint(Eu, None, Some(&[Eu, Us])), None);
        assert_eq!(hint(Eu, None, Some(&[Us, Kr])), None);
        assert_eq!(hint(Eu, None, Some(&[])), None);
        assert_eq!(hint(Eu, None, None), None);
        // The portal names another region.
        assert_eq!(hint(Eu, Some("us"), Some(&[Eu])), Some(Us));
        assert_eq!(hint(Eu, Some("us"), None), Some(Us));
        assert_eq!(hint(Us, Some("us"), Some(&[])), None);
        // A KR portal with a saved TW is no mismatch, unless only KR knows the realm.
        assert_eq!(hint(Tw, Some("kr"), Some(&[Tw])), None);
        assert_eq!(hint(Tw, Some("kr"), Some(&[Kr])), Some(Kr));
        assert_eq!(hint(Tw, Some("kr"), Some(&[])), None);
        assert_eq!(hint(Eu, Some("kr"), Some(&[Tw])), Some(Tw));
        assert_eq!(hint(Eu, Some("kr"), Some(&[])), Some(Kr));
    }

    /// A resolve-realm stand on a local port: 200 for a region in `knows`, 500 for one in
    /// `fails`, 404 otherwise. Hands back the base URL and every request line it saw.
    pub(crate) fn realm_stand(knows: &[&str], fails: &[&str]) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = seen.clone();
        let knows: Vec<String> = knows.iter().map(|r| format!("region={r}&")).collect();
        let fails: Vec<String> = fails.iter().map(|r| format!("region={r}&")).collect();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut request = Vec::new();
                let mut buf = [0u8; 4096];
                while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                    match stream.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => request.extend_from_slice(&buf[..n]),
                    }
                }
                let line = String::from_utf8_lossy(&request).lines().next().unwrap_or("").to_string();
                let (status, body) = if knows.iter().any(|k| line.contains(k)) {
                    ("200 OK", r#"{"slug":"stormrage"}"#)
                } else if fails.iter().any(|k| line.contains(k)) {
                    ("500 Internal Server Error", "{}")
                } else {
                    ("404 Not Found", r#"{"error":"not_found"}"#)
                };
                log.lock().unwrap().push(line);
                let _ = stream.write_all(
                    format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes(),
                );
            }
        });
        (base, seen)
    }

    /// A `_retail_` folder with one realm folder and, when given, a Config.wtf portal line.
    pub(crate) fn retail_dir(label: &str, portal: Option<&str>, realm: Option<&str>) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("goldcap-realm-region-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("WTF").join("Account").join("ACC")).unwrap();
        if let Some(p) = portal {
            std::fs::write(dir.join("WTF").join("Config.wtf"), format!("SET portal \"{p}\"\n")).unwrap();
        }
        if let Some(r) = realm {
            std::fs::create_dir_all(dir.join("WTF").join("Account").join("ACC").join(r)).unwrap();
        }
        dir
    }

    async fn detect(label: &str, portal: Option<&str>, realm: Option<&str>, knows: &[&str], fails: &[&str]) -> (Option<Region>, Vec<String>) {
        let dir = retail_dir(label, portal, realm);
        let (base, seen) = realm_stand(knows, fails);
        let region = detect_at(&crate::sync::build_client(), &base, &dir).await;
        std::fs::remove_dir_all(&dir).ok();
        let seen = seen.lock().unwrap().clone();
        (region, seen)
    }

    #[tokio::test]
    async fn the_wizard_takes_a_us_or_eu_portal_without_asking_goldcap() {
        let (region, seen) = detect("portal-us", Some("US"), Some("Stormrage"), &["eu", "us"], &[]).await;
        assert_eq!((region, seen.len()), (Some(Us), 0));
    }

    #[tokio::test]
    async fn the_wizard_reads_a_kr_portal_as_kr_or_tw_by_the_realm() {
        let (region, seen) = detect("portal-kr-tw", Some("KR"), Some("Shadowmoon TW"), &["tw"], &[]).await;
        assert_eq!(region, Some(Tw));
        assert!(seen.iter().any(|l| l.contains("region=kr&")) && seen.iter().any(|l| l.contains("region=tw&")), "{seen:?}");
        let (region, _) = detect("portal-kr-kr", Some("KR"), Some("Azshara KR"), &["kr"], &[]).await;
        assert_eq!(region, Some(Kr));
    }

    #[tokio::test]
    async fn the_wizard_with_no_portal_takes_the_one_region_that_knows_the_realm() {
        let (region, seen) = detect("noportal-us", None, Some("Area 52"), &["us"], &[]).await;
        assert_eq!(region, Some(Us));
        for r in ["us", "eu", "kr", "tw"] {
            assert!(seen.iter().any(|l| l.contains(&format!("region={r}&"))), "{r}: {seen:?}");
        }
    }

    #[tokio::test]
    async fn the_wizard_with_no_portal_asks_when_several_none_or_no_answer() {
        assert_eq!(detect("noportal-both", None, Some("Stormrage"), &["us", "eu"], &[]).await.0, None);
        assert_eq!(detect("noportal-none", None, Some("Nowhere"), &[], &[]).await.0, None);
        assert_eq!(detect("noportal-norealm", None, None, &["eu"], &[]).await.0, None);
        // EU knows it, but US could not be asked: that is not "only EU".
        assert_eq!(detect("noportal-down", None, Some("Tarren Mill"), &["eu"], &["us"]).await.0, None);
    }
}
