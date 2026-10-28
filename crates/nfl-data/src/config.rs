use std::time::Duration;

use utils::Secret;

#[derive(Debug, Clone)]
pub struct NflDataConfig {
    pub nflverse_database_url: String,
    pub fanduel_database_url: String,
    pub database_url: String,
    /// Datasets fetch seasons >= this.
    pub earliest_season: u16,
    pub github_token: Option<Secret>,
    pub github_api_base: String,
    /// Background refresh cadence; `None` disables it.
    pub refresh_interval: Option<Duration>,
}

impl From<NflDataConfig> for nflverse_data::NflverseDataConfig {
    fn from(config: NflDataConfig) -> Self {
        Self {
            database_url: config.nflverse_database_url,
            earliest_season: config.earliest_season,
            github_token: config.github_token,
            github_api_base: config.github_api_base,
        }
    }
}

impl Default for NflDataConfig {
    fn default() -> Self {
        let config = nflverse_data::NflverseDataConfig::default();
        Self {
            nflverse_database_url: config.database_url,
            fanduel_database_url: "sqlite://./storage/fanduel-data.db".to_owned(),
            database_url: "sqlite://./storage/nfl-data.db".to_owned(),
            earliest_season: config.earliest_season,
            github_token: config.github_token,
            github_api_base: config.github_api_base,
            refresh_interval: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::NflDataConfig;
    use utils::Secret;

    #[test]
    fn config_defaults_to_manual_refresh() {
        assert!(NflDataConfig::default().refresh_interval.is_none());
    }

    #[test]
    fn defaults_match_documented_storage_layout() {
        let config = NflDataConfig::default();
        assert_eq!(
            config.fanduel_database_url,
            "sqlite://./storage/fanduel-data.db"
        );
        assert_eq!(config.database_url, "sqlite://./storage/nfl-data.db");
        assert_eq!(config.refresh_interval, None);
    }

    #[test]
    fn provider_conversion_preserves_provider_configuration() {
        let provider: nflverse_data::NflverseDataConfig = NflDataConfig {
            nflverse_database_url: "sqlite://cache.db".into(),
            fanduel_database_url: "sqlite://fanduel.db".into(),
            database_url: "sqlite://nfl-data.db".into(),
            earliest_season: 2020,
            github_token: Some(Secret::new("sentinel")),
            github_api_base: "https://api.example.test".into(),
            refresh_interval: Some(std::time::Duration::from_secs(30)),
        }
        .into();
        assert_eq!(provider.database_url, "sqlite://cache.db");
        assert_eq!(provider.earliest_season, 2020);
        assert_eq!(provider.github_token.unwrap().expose(), "sentinel");
        assert_eq!(provider.github_api_base, "https://api.example.test");
    }
}
