//! The signed manifest the addon release workflow publishes at
//! `goldcap.gg/downloads/addon/addon.json`, and the checks a downloaded addon
//! zip must pass before anything is unpacked.

use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;

pub const MANIFEST_URL: &str = "https://goldcap.gg/downloads/addon/addon.json";
pub const URL_PREFIX: &str = "https://goldcap.gg/downloads/addon/";

/// `major.minor.minor` exactly, compared numerically. Anything else (a
/// pre-release, a packager placeholder, a short form) has no version, and a
/// folder with no version is never touched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version(pub u32, pub u32, pub u32);

impl Version {
    /// Accepts `1.2.3`, `v1.2.3` and `addon-v1.2.3`.
    pub fn parse(s: &str) -> Option<Version> {
        let s = s.trim();
        let s = s.strip_prefix("addon-v").or_else(|| s.strip_prefix('v')).unwrap_or(s);
        let mut parts = s.split('.');
        let mut next = || {
            let p = parts.next()?;
            (!p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
                .then(|| p.parse::<u32>().ok())
                .flatten()
        };
        let v = Version(next()?, next()?, next()?);
        parts.next().is_none().then_some(v)
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddonManifest {
    pub version: String,
    pub url: String,
    pub sha256: String,
    pub size: u64,
    pub published_at: String,
}

#[derive(Debug, Clone)]
pub struct VerifiedRelease {
    pub manifest: AddonManifest,
    pub version: Version,
    pub zip: Vec<u8>,
}

/// The self-updater's public key, read from the same config the updater plugin
/// reads, so there is one key to rotate.
pub fn updater_pubkey() -> &'static str {
    static KEY: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    KEY.get_or_init(|| {
        let conf: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap_or_default();
        conf["plugins"]["updater"]["pubkey"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    })
}

fn decode_text(b64: &str) -> Result<String, String> {
    let raw = base64::engine::general_purpose::STANDARD
        .decode(b64.trim())
        .map_err(|e| format!("not base64: {e}"))?;
    String::from_utf8(raw).map_err(|e| format!("not text: {e}"))
}

/// Checks the signature over the exact bytes, then the contents.
pub fn verify_manifest(
    json: &[u8],
    sig_b64: &str,
    pubkey_b64: &str,
    url_prefix: &str,
) -> Result<(AddonManifest, Version), String> {
    let key = minisign_verify::PublicKey::decode(
        &decode_text(pubkey_b64).map_err(|e| format!("public key: {e}"))?,
    )
    .map_err(|e| format!("public key: {e}"))?;
    let sig = minisign_verify::Signature::decode(
        &decode_text(sig_b64).map_err(|e| format!("signature: {e}"))?,
    )
    .map_err(|e| format!("signature: {e}"))?;
    key.verify(json, &sig, true)
        .map_err(|e| format!("signature does not match the manifest: {e}"))?;
    let manifest: AddonManifest =
        serde_json::from_slice(json).map_err(|e| format!("manifest is not valid: {e}"))?;
    let version = validate(&manifest, url_prefix)?;
    Ok((manifest, version))
}

/// The manifest's own fields: a version that parses and a zip on our own host.
pub fn validate(m: &AddonManifest, url_prefix: &str) -> Result<Version, String> {
    let version = Version::parse(&m.version)
        .ok_or_else(|| format!("manifest version {:?} cannot be read", m.version))?;
    if !m.url.starts_with(url_prefix) || !m.url.ends_with(".zip") {
        return Err(format!("manifest url {:?} is not on {url_prefix}", m.url));
    }
    Ok(version)
}

/// The downloaded bytes are the ones the manifest names.
pub fn check_payload(zip: &[u8], m: &AddonManifest) -> Result<(), String> {
    if zip.len() as u64 != m.size {
        return Err(format!("zip size {} does not match the manifest's {}", zip.len(), m.size));
    }
    let hex = format!("{:x}", Sha256::digest(zip));
    if !hex.eq_ignore_ascii_case(m.sha256.trim()) {
        return Err("zip sha256 does not match the manifest".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const MANIFEST: &[u8] = include_bytes!("../tests/fixtures/addon/addon.json");
    const SIG: &str = include_str!("../tests/fixtures/addon/addon.json.sig");
    const TEST_PUB: &str = include_str!("../tests/fixtures/addon/test.pub");
    const TAMPERED: &[u8] = include_bytes!("../tests/fixtures/addon/tampered.json");

    #[test]
    fn version_parses_tag_forms() {
        assert_eq!(Version::parse("addon-v0.22.0"), Some(Version(0, 22, 0)));
        assert_eq!(Version::parse("v1.2.3"), Some(Version(1, 2, 3)));
        assert_eq!(Version::parse("1.2.3"), Some(Version(1, 2, 3)));
        for bad in ["1.2", "1.2.3-rc1", "@project-version@", "", "1.2.3.4"] {
            assert_eq!(Version::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn versions_compare_numerically() {
        assert!(Version(0, 9, 0) < Version(0, 10, 0));
        assert_eq!(Version(0, 22, 0).to_string(), "0.22.0");
    }

    #[test]
    fn a_signed_manifest_verifies() {
        let (m, v) = verify_manifest(MANIFEST, SIG, TEST_PUB, URL_PREFIX).unwrap();
        assert_eq!(v, Version(0, 23, 0));
        assert_eq!(m.size, 8);
    }

    #[test]
    fn a_tampered_manifest_is_refused() {
        assert!(verify_manifest(TAMPERED, SIG, TEST_PUB, URL_PREFIX).is_err());
    }

    #[test]
    fn a_signature_from_another_key_is_refused() {
        assert!(verify_manifest(MANIFEST, SIG, updater_pubkey(), URL_PREFIX).is_err());
    }

    #[test]
    fn a_manifest_pointing_off_site_is_refused() {
        let mut m = AddonManifest {
            version: "0.23.0".into(),
            url: "https://evil.example/x.zip".into(),
            sha256: String::new(),
            size: 1,
            published_at: String::new(),
        };
        assert!(validate(&m, URL_PREFIX).is_err());
        m.url = "http://goldcap.gg/downloads/addon/x.zip".into();
        assert!(validate(&m, URL_PREFIX).is_err());
        m.url = "https://goldcap.gg/downloads/addon/x.zip".into();
        assert!(validate(&m, URL_PREFIX).is_ok());
    }

    #[test]
    fn payload_must_match_size_and_hash() {
        let (m, _) = verify_manifest(MANIFEST, SIG, TEST_PUB, URL_PREFIX).unwrap();
        assert!(check_payload(b"zipbytes", &m).is_ok());
        assert!(check_payload(b"zipbytez", &m).unwrap_err().contains("sha256"));
        assert!(check_payload(b"zipbyte", &m).unwrap_err().contains("size"));
    }

    #[test]
    fn the_embedded_updater_key_decodes() {
        use base64::Engine;
        let raw = base64::engine::general_purpose::STANDARD
            .decode(updater_pubkey())
            .unwrap();
        let text = String::from_utf8(raw).unwrap();
        assert!(minisign_verify::PublicKey::decode(&text).is_ok());
    }
}
