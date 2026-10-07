use time::OffsetDateTime;

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

impl Default for NflDataConfig {
    fn default() -> Self {
        Self {
            database_url: "sqlite://storage/nflverse-cache.db".into(),
            earliest_season: (OffsetDateTime::now_utc().year() - 2) as u16,
            github_token: None,
            github_api_base: "https://api.github.com".into(),
        }
    }
}
