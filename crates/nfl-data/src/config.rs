use std::time::Duration;

use utils::Secret;

#[derive(Debug, Clone)]
pub struct NflDataConfig {
    pub database_url: String,
    pub fanduel_database_url: String,
    /// Per-season datasets fetch seasons >= this. Lower bound only, so the
    /// exact NFL season boundary doesn't matter.
    pub earliest_season: u16,
    /// Optional token to raise the GitHub API rate limit. Never required.
    pub github_token: Option<Secret>,
    /// Overridable so tests can point at a mock server.
    pub github_api_base: String,
    /// Configured cadence for internal background refreshes; `None` disables it.
    pub refresh_interval: Option<Duration>,
}

impl From<NflDataConfig> for nflverse_data::NflverseDataConfig {
    fn from(config: NflDataConfig) -> Self {
        Self {
            database_url: config.database_url,
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
            database_url: config.database_url,
            fanduel_database_url: "sqlite://./storage/fanduel-data.db".to_owned(),
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
    fn provider_conversion_preserves_provider_configuration() {
        let provider: nflverse_data::NflverseDataConfig = NflDataConfig {
            database_url: "sqlite://cache.db".into(),
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
