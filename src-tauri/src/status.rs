//! Assembles the Status screen's view of the whole pipeline:
//! `goldcap.gg -> companion -> addon` for prices, and `addon -> companion ->
//! goldcap.gg` for the ledger. Pure and time-injected so every state can be
//! tested without a running app.

use crate::config::Config;
use crate::health::GameHealth;
use crate::sync::{SyncErrorStage, SyncStatus};
use serde::Serialize;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum StageHealth {
    Ok,
    /// Nothing is wrong — this link simply is not hooked up yet. Not paired,
    /// or the game has never written a ledger. Must not read as an error.
    NotConnected,
    Broken,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StageState {
    pub state: StageHealth,
    /// Unix seconds of this stage's last success. The UI formats it and keeps
    /// re-rendering, so the phrase in `detail` carries no time of its own.
    pub at: Option<i64>,
    pub detail: String,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusSnapshot {
    pub configured: bool,
    pub paired: bool,
    pub region: String,
    pub realm_slug: String,
    pub syncing: bool,
    pub next_tick_at: Option<i64>,
    pub interval_minutes: u32,
    pub prices: StageState,
    pub addon: StageState,
    pub ledger: StageState,
    pub version: String,
}

fn unix(at: SystemTime) -> Option<i64> {
    at.duration_since(UNIX_EPOCH).ok().map(|d| d.as_secs() as i64)
}

fn stage(state: StageHealth, at: Option<i64>, detail: &str, error: Option<String>) -> StageState {
    StageState { state, at, detail: detail.to_string(), error }
}

pub fn build(
    config: &Config,
    status: &SyncStatus,
    health: &GameHealth,
    _now: i64,
) -> StatusSnapshot {
    let configured = config.is_complete();
    let paired = !config.companion_token.trim().is_empty();

    let prices = if !configured {
        stage(StageHealth::NotConnected, None, "Waiting for setup", None)
    } else if status.last_error_stage == Some(SyncErrorStage::Prices) {
        // A broken link is still a link that worked at some point — keep
        // showing when, the same way the ledger stage below does, rather
        // than dropping it the moment this tick fails.
        stage(
            StageHealth::Broken,
            status.last_success_at.and_then(unix),
            "Could not reach goldcap.gg",
            status.last_error.clone(),
        )
    } else if let Some(at) = status.last_success_at.and_then(unix) {
        stage(StageHealth::Ok, Some(at), "Fetched from goldcap.gg", None)
    } else {
        stage(StageHealth::NotConnected, None, "No sync yet", None)
    };

    let addon = if !configured {
        stage(StageHealth::NotConnected, None, "Waiting for setup", None)
    } else if !health.addon_installed {
        stage(
            StageHealth::Broken,
            None,
            "The GoldCap addon is not installed",
            None,
        )
    } else if status.last_error_stage == Some(SyncErrorStage::Addon) {
        // The fetch worked (that is exactly what puts an error in this
        // stage instead of Prices) — this is a write problem specifically.
        // `install_ok` (WTF/Account exists) separates a real, played
        // install that just failed to write from a folder that was never
        // actually run: the former is worth troubleshooting as a broken
        // link, the latter is more likely the wrong folder entirely.
        let detail = if health.install_ok {
            "Could not write price data"
        } else {
            "This doesn't look like a played WoW install"
        };
        stage(
            StageHealth::Broken,
            health.app_data_written_at,
            detail,
            status.last_error.clone(),
        )
    } else if let Some(at) = health.app_data_written_at {
        stage(StageHealth::Ok, Some(at), "Written to GoldCap_AppData", None)
    } else {
        stage(StageHealth::NotConnected, None, "No price file written yet", None)
    };

    let ledger = if !configured {
        stage(StageHealth::NotConnected, None, "Waiting for setup", None)
    } else if !paired {
        stage(
            StageHealth::NotConnected,
            None,
            "Not paired — nothing is uploaded",
            None,
        )
    } else if let Some(err) = &status.upload.last_error {
        stage(
            StageHealth::Broken,
            status.upload.last_upload_at,
            "Upload was refused",
            Some(err.clone()),
        )
    } else if health.saved_vars_written_at.is_none() {
        stage(
            StageHealth::NotConnected,
            None,
            "No ledger yet — launch WoW with GoldCap",
            None,
        )
    } else {
        let queue = if status.upload.pending == 0 {
            "queue empty".to_string()
        } else {
            format!("{} queued", status.upload.pending)
        };
        stage(
            StageHealth::Ok,
            status.upload.last_upload_at,
            &format!("{} rows sent · {queue}", status.upload.total_sent),
            None,
        )
    };

    StatusSnapshot {
        configured,
        paired,
        region: config.region.to_string(),
        realm_slug: config.realm_slug.clone(),
        syncing: status.syncing,
        next_tick_at: status.next_tick_at.and_then(unix),
        interval_minutes: config.interval_minutes,
        prices,
        addon,
        ledger,
        version: env!("CARGO_PKG_VERSION").to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, Region};
    use crate::health::GameHealth;
    use crate::sync::{SyncErrorStage, SyncStatus};
    use std::time::{Duration, SystemTime};

    const NOW: i64 = 1_785_600_000;

    fn configured() -> Config {
        Config {
            region: Region::Eu,
            realm_slug: "dentarg".into(),
            wow_retail_path: "/tmp/wow/_retail_".into(),
            ..Config::default()
        }
    }

    fn healthy() -> GameHealth {
        GameHealth {
            install_ok: true,
            addon_installed: true,
            app_data_written_at: Some(NOW - 240),
            saved_vars_written_at: Some(NOW - 3600),
        }
    }

    #[test]
    fn an_unconfigured_companion_reports_every_stage_as_not_connected() {
        let snap = build(&Config::default(), &SyncStatus::default(), &GameHealth::default(), NOW);
        assert!(!snap.configured);
        assert_eq!(snap.prices.state, StageHealth::NotConnected);
        assert_eq!(snap.addon.state, StageHealth::NotConnected);
        assert_eq!(snap.ledger.state, StageHealth::NotConnected);
    }

    #[test]
    fn a_healthy_paired_companion_reports_three_green_stages() {
        let mut config = configured();
        config.companion_token = "tok".into();
        let status = SyncStatus {
            last_success_at: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(NOW as u64 - 240)),
            last_success_realm: Some("dentarg".into()),
            upload: crate::upload::UploadStats {
                total_sent: 142,
                last_upload_at: Some(NOW - 240),
                pending: 0,
                last_error: None,
            },
            ..SyncStatus::default()
        };

        let snap = build(&config, &status, &healthy(), NOW);
        assert!(snap.configured);
        assert!(snap.paired);
        assert_eq!(snap.prices.state, StageHealth::Ok);
        assert_eq!(snap.addon.state, StageHealth::Ok);
        assert_eq!(snap.ledger.state, StageHealth::Ok);
        assert_eq!(snap.prices.at, Some(NOW - 240));
        assert_eq!(snap.addon.at, Some(NOW - 240));
        assert!(snap.ledger.detail.contains("142"));
    }

    #[test]
    fn a_fetch_error_breaks_only_the_prices_stage() {
        let mut config = configured();
        config.companion_token = "tok".into();
        let status = SyncStatus {
            last_error: Some("unexpected status 500".into()),
            last_error_stage: Some(SyncErrorStage::Prices),
            upload: crate::upload::UploadStats { total_sent: 1, ..Default::default() },
            ..SyncStatus::default()
        };

        let snap = build(&config, &status, &healthy(), NOW);
        assert_eq!(snap.prices.state, StageHealth::Broken);
        assert_eq!(snap.prices.error.as_deref(), Some("unexpected status 500"));
        assert_eq!(snap.addon.state, StageHealth::Ok, "a stale file is still a written file");
        assert_eq!(snap.ledger.state, StageHealth::Ok);
    }

    #[test]
    fn a_write_failure_attributes_to_the_addon_stage_not_prices() {
        // The fetch succeeded this tick — apply_import_string is what threw
        // — so prices must read as fine and the failure must land on addon.
        let mut config = configured();
        config.companion_token = "tok".into();
        let status = SyncStatus {
            last_success_at: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(NOW as u64 - 30)),
            last_success_realm: Some("dentarg".into()),
            last_error: Some("failed to write addon files: permission denied".into()),
            last_error_stage: Some(SyncErrorStage::Addon),
            ..SyncStatus::default()
        };

        let snap = build(&config, &status, &healthy(), NOW);
        assert_eq!(
            snap.prices.state,
            StageHealth::Ok,
            "the site was reached fine — only the addon write failed"
        );
        assert_eq!(snap.addon.state, StageHealth::Broken);
        assert!(snap.addon.error.as_deref().unwrap().contains("permission denied"));
    }

    #[test]
    fn a_write_failure_on_a_never_played_folder_says_so() {
        let mut config = configured();
        config.companion_token = "tok".into();
        let health = GameHealth { install_ok: false, ..healthy() };
        let status = SyncStatus {
            last_success_at: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(NOW as u64 - 30)),
            last_error: Some("failed to write addon files: not found".into()),
            last_error_stage: Some(SyncErrorStage::Addon),
            ..SyncStatus::default()
        };

        let snap = build(&config, &status, &health, NOW);
        assert_eq!(snap.addon.state, StageHealth::Broken);
        assert!(snap.addon.detail.contains("played"));
    }

    #[test]
    fn a_broken_prices_stage_still_reports_its_last_good_timestamp() {
        let config = configured();
        let last_good = NOW - 5_000;
        let status = SyncStatus {
            last_success_at: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(last_good as u64)),
            last_success_realm: Some("dentarg".into()),
            last_error: Some("request failed: connection refused".into()),
            last_error_stage: Some(SyncErrorStage::Prices),
            ..SyncStatus::default()
        };

        let snap = build(&config, &status, &healthy(), NOW);
        assert_eq!(snap.prices.state, StageHealth::Broken);
        assert_eq!(
            snap.prices.at,
            Some(last_good),
            "a broken link should still show when it last worked, same as the ledger stage does"
        );
    }

    #[test]
    fn a_missing_addon_breaks_the_addon_stage_even_when_prices_arrive() {
        let health = GameHealth { addon_installed: false, ..healthy() };
        let snap = build(&configured(), &SyncStatus::default(), &health, NOW);
        assert_eq!(snap.addon.state, StageHealth::Broken);
        assert!(snap.addon.detail.contains("not installed"));
    }

    #[test]
    fn an_unpaired_ledger_is_not_connected_rather_than_broken() {
        let snap = build(&configured(), &SyncStatus::default(), &healthy(), NOW);
        assert_eq!(
            snap.ledger.state,
            StageHealth::NotConnected,
            "choosing not to pair is not a failure"
        );
        assert!(!snap.paired);
    }

    #[test]
    fn a_paired_companion_that_has_never_seen_the_game_says_so() {
        let mut config = configured();
        config.companion_token = "tok".into();
        let health = GameHealth { saved_vars_written_at: None, ..healthy() };

        let snap = build(&config, &SyncStatus::default(), &health, NOW);
        assert_eq!(snap.ledger.state, StageHealth::NotConnected);
        assert!(snap.ledger.detail.contains("launch WoW"));
    }

    #[test]
    fn an_upload_error_breaks_the_ledger_stage() {
        let mut config = configured();
        config.companion_token = "tok".into();
        let status = SyncStatus {
            upload: crate::upload::UploadStats {
                total_sent: 10,
                pending: 4,
                last_error: Some("unexpected status 503".into()),
                ..Default::default()
            },
            ..SyncStatus::default()
        };

        let snap = build(&config, &status, &healthy(), NOW);
        assert_eq!(snap.ledger.state, StageHealth::Broken);
        assert_eq!(snap.ledger.error.as_deref(), Some("unexpected status 503"));
    }

    #[test]
    fn the_in_flight_flag_and_schedule_survive_serialization() {
        let status = SyncStatus {
            syncing: true,
            next_tick_at: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(NOW as u64 + 1560)),
            ..SyncStatus::default()
        };
        let snap = build(&configured(), &status, &healthy(), NOW);
        assert!(snap.syncing);
        assert_eq!(snap.next_tick_at, Some(NOW + 1560));

        let json = serde_json::to_string(&snap).unwrap();
        assert!(json.contains("\"realmSlug\":\"dentarg\""));
        assert!(json.contains("\"nextTickAt\""));
        assert!(json.contains("\"notConnected\"") || json.contains("\"ok\""));
    }
}
