use utils::Secret;

#[derive(Debug, Clone)]
pub struct NflDataConfig {
    pub database_url: String,
    /// Per-season datasets fetch seasons >= this. Lower bound only, so the
    /// exact NFL season boundary doesn't matter.
    pub earliest_season: u16,
    /// Optional token to raise the GitHub API rate limit. Never required.
    pub github_token: Option<Secret>,
    /// Overridable so tests can point at a mock server.
    pub github_api_base: String,
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
            earliest_season: config.earliest_season,
            github_token: config.github_token,
            github_api_base: config.github_api_base,
        }
    }
}
