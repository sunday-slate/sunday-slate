use serde::Deserialize;
use std::path::PathBuf;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::mail::transport::SmtpConfig;
use utils::Secret;

fn default_database_url() -> String {
    "sqlite://./storage/sunday-slate.db".to_string()
}
fn default_base_url() -> String {
    "http://localhost:3000".to_string()
}
fn default_bind_addr() -> String {
    "127.0.0.1:3000".to_string()
}
fn default_mail_from() -> String {
    "Sunday Slate <no-reply@example.com>".to_string()
}
fn default_nflverse_database_url() -> String {
    "sqlite://./storage/nflverse-data.db".to_string()
}
fn default_season() -> u16 {
    2026
}
fn default_media_dir() -> PathBuf {
    PathBuf::from("./storage/media")
}
fn default_live_dev_tick_ms() -> u64 {
    3000
}

fn normalize_base_url(url: &str) -> String {
    url.trim_end_matches('/').to_string()
}

fn parse_now(s: &str) -> anyhow::Result<OffsetDateTime> {
    OffsetDateTime::parse(s, &Rfc3339)
        .map_err(|e| anyhow::anyhow!("invalid SUNDAY_SLATE_NOW {s:?}: {e}"))
}

#[derive(Debug, Deserialize, Clone)]
pub struct Config {
    #[serde(default = "default_database_url")]
    pub database_url: String,

    #[serde(default = "default_bind_addr")]
    pub bind_addr: String,

    #[serde(default = "default_base_url")]
    pub base_url: String,

    #[serde(default = "default_mail_from")]
    pub mail_from: String,

    #[serde(default = "default_nflverse_database_url")]
    pub nflverse_database_url: String,

    #[serde(default)]
    pub nflverse_github_token: Option<Secret>,
    #[serde(default)]
    pub tank01_api_key: Option<Secret>,
    #[serde(default)]
    pub live_dev_feed: bool,
    #[serde(default = "default_live_dev_tick_ms")]
    pub live_dev_feed_tick_ms: u64,
    #[serde(default)]
    pub nflverse_sync_interval_secs: u64,
    #[serde(default = "default_season")]
    pub season: u16,

    #[serde(default = "default_media_dir")]
    pub media_dir: PathBuf,

    #[serde(default)]
    pub smtp: Option<SmtpConfig>,

    #[serde(skip)]
    pub now_override: Option<OffsetDateTime>,
}
fn environment_source() -> config::Environment {
    config::Environment::with_prefix("SUNDAY_SLATE")
        .prefix_separator("__")
        .separator("__")
}

impl Config {
    pub fn load() -> anyhow::Result<Self> {
        let cfg = config::Config::builder()
            .add_source(config::File::with_name("config").required(false))
            .add_source(environment_source())
            .build()?;
        let mut cfg: Self = cfg.try_deserialize()?;
        cfg.base_url = normalize_base_url(&cfg.base_url);
        cfg.now_override = match std::env::var("SUNDAY_SLATE_NOW") {
            Ok(s) => Some(parse_now(&s)?),
            Err(_) => None,
        };
        Ok(cfg)
    }

    pub fn session_database_url(&self) -> String {
        if let Some(stripped) = self.database_url.strip_suffix(".db") {
            format!("{stripped}-sessions.db")
        } else {
            format!("{}-sessions", self.database_url)
        }
    }

    pub fn now(&self) -> OffsetDateTime {
        self.now_override.unwrap_or_else(OffsetDateTime::now_utc)
    }

    /// Automatic nflverse-sync cadence, or None when disabled.
    pub fn nflverse_sync_interval(&self) -> Option<std::time::Duration> {
        (self.nflverse_sync_interval_secs > 0)
            .then(|| std::time::Duration::from_secs(self.nflverse_sync_interval_secs))
    }
}

#[cfg(test)]
mod tests {
    use super::{environment_source, normalize_base_url};
    use std::collections::HashMap;
    use time::OffsetDateTime;
    use time::macros::datetime;

    #[test]
    fn prefixed_environment_maps_to_config_fields() {
        let source = HashMap::from([
            ("SUNDAY_SLATE__BIND_ADDR".into(), "0.0.0.0:8080".into()),
            ("BIND_ADDR".into(), "127.0.0.1:9999".into()),
            ("SUNDAY_SLATE__SEASON".into(), "2025".into()),
            ("SUNDAY_SLATE__SMTP__HOST".into(), "smtp.example.com".into()),
            ("SUNDAY_SLATE__SMTP__PORT".into(), "2525".into()),
            ("SUNDAY_SLATE__SMTP__USERNAME".into(), "slate".into()),
            ("SUNDAY_SLATE__SMTP__PASSWORD".into(), "secret".into()),
            ("SUNDAY_SLATE__LIVE_DEV_FEED".into(), "true".into()),
            ("SUNDAY_SLATE__LIVE_DEV_FEED_TICK_MS".into(), "1500".into()),
            (
                "SUNDAY_SLATE__NFLVERSE_SYNC_INTERVAL_SECS".into(),
                "3600".into(),
            ),
            (
                "SUNDAY_SLATE__NFLVERSE_DATABASE_URL".into(),
                "sqlite://cache.db".into(),
            ),
            (
                "SUNDAY_SLATE__NFLVERSE_GITHUB_TOKEN".into(),
                "nfl-token-sentinel".into(),
            ),
        ]);
        let cfg: super::Config = config::Config::builder()
            .add_source(environment_source().source(Some(source)))
            .build()
            .unwrap()
            .try_deserialize()
            .unwrap();

        assert_eq!(cfg.bind_addr, "0.0.0.0:8080");
        assert_eq!(cfg.season, 2025);
        assert!(cfg.live_dev_feed);
        assert_eq!(cfg.live_dev_feed_tick_ms, 1500);
        assert_eq!(cfg.nflverse_sync_interval_secs, 3600);
        assert_eq!(
            cfg.nflverse_sync_interval(),
            Some(std::time::Duration::from_secs(3600))
        );
        assert_eq!(cfg.nflverse_database_url, "sqlite://cache.db");
        assert_eq!(
            cfg.nflverse_github_token.as_ref().unwrap().expose(),
            "nfl-token-sentinel"
        );
        let debug = format!("{cfg:?}");
        assert!(!debug.contains("nfl-token-sentinel"));
        assert!(!debug.contains("secret"));
        let smtp = cfg.smtp.expect("prefixed SMTP configuration");
        assert_eq!(smtp.host, "smtp.example.com");
        assert_eq!(smtp.port, 2525);
        assert_eq!(smtp.username, "slate");
        assert_eq!(smtp.password.expose(), "secret");
    }

    #[test]
    fn nflverse_sync_interval_defaults_to_disabled() {
        let cfg: super::Config = config::Config::builder()
            .add_source(environment_source().source(Some(HashMap::new())))
            .build()
            .unwrap()
            .try_deserialize()
            .unwrap();

        assert_eq!(cfg.nflverse_sync_interval_secs, 0);
        assert_eq!(cfg.nflverse_sync_interval(), None);
    }

    #[test]
    fn nflverse_sync_interval_zero_is_off() {
        let cfg: super::Config = config::Config::builder()
            .add_source(environment_source().source(Some(HashMap::from([(
                "SUNDAY_SLATE__NFLVERSE_SYNC_INTERVAL_SECS".into(),
                "0".into(),
            )]))))
            .build()
            .unwrap()
            .try_deserialize()
            .unwrap();

        assert_eq!(cfg.nflverse_sync_interval_secs, 0);
        assert_eq!(cfg.nflverse_sync_interval(), None);
    }

    #[test]
    fn legacy_nfl_settings_have_no_aliases() {
        let cfg: super::Config = config::Config::builder()
            .add_source(environment_source().source(Some(HashMap::from([
                (
                    "SUNDAY_SLATE__NFL_DATABASE_URL".into(),
                    "sqlite://legacy.db".into(),
                ),
                (
                    "SUNDAY_SLATE__NFL_GITHUB_TOKEN".into(),
                    "legacy-token".into(),
                ),
                ("SUNDAY_SLATE__NFL_SYNC_INTERVAL_SECS".into(), "3600".into()),
            ]))))
            .build()
            .unwrap()
            .try_deserialize()
            .unwrap();
        assert_eq!(
            cfg.nflverse_database_url,
            "sqlite://./storage/nflverse-data.db"
        );
        assert!(cfg.nflverse_github_token.is_none());
        assert_eq!(cfg.nflverse_sync_interval(), None);
    }

    #[test]
    fn normalize_base_url_strips_trailing_slashes() {
        assert_eq!(
            normalize_base_url("http://localhost:3000/"),
            "http://localhost:3000"
        );
        assert_eq!(
            normalize_base_url("http://localhost:3000"),
            "http://localhost:3000"
        );
        assert_eq!(
            normalize_base_url("http://localhost:3000///"),
            "http://localhost:3000"
        );
    }

    fn sample_config() -> super::Config {
        super::Config {
            database_url: "sqlite://./storage/sunday-slate.db".into(),
            bind_addr: "127.0.0.1:3000".into(),
            base_url: "http://localhost:3000".into(),
            mail_from: "Sunday Slate <no-reply@example.com>".into(),
            nflverse_database_url: "sqlite://./storage/nflverse-data.db".into(),
            nflverse_github_token: None,
            tank01_api_key: None,
            live_dev_feed: false,
            live_dev_feed_tick_ms: 3000,
            nflverse_sync_interval_secs: 0,
            season: 2025,
            media_dir: "./storage/media".into(),
            smtp: None,
            now_override: None,
        }
    }

    #[test]
    fn now_uses_override_when_set() {
        let mut cfg = sample_config();
        cfg.now_override = Some(datetime!(2025-09-02 08:00 UTC));
        assert_eq!(cfg.now(), datetime!(2025-09-02 08:00 UTC));
    }

    #[test]
    fn now_falls_back_to_system_clock() {
        let cfg = sample_config();
        let before = OffsetDateTime::now_utc();
        let got = cfg.now();
        let after = OffsetDateTime::now_utc();
        assert!(got >= before && got <= after);
    }

    #[test]
    fn parse_now_accepts_rfc3339_and_rejects_junk() {
        assert_eq!(
            super::parse_now("2025-09-02T08:00:00Z").unwrap(),
            datetime!(2025-09-02 08:00 UTC)
        );
        assert!(super::parse_now("not-a-time").is_err());
    }

    #[test]
    fn default_media_dir_is_under_storage() {
        assert_eq!(
            super::default_media_dir(),
            std::path::PathBuf::from("./storage/media")
        );
    }
}
