use std::time::Duration;

/// Configuration for `NflData`'s own durable store and the FanDuel provider.
/// The nflverse provider is constructed by the caller and passed to
/// `NflData::connect`, so this config carries no nflverse settings.
#[derive(Debug, Clone)]
pub struct NflDataConfig {
    pub fanduel_database_url: String,
    /// Durable nfl-data-owned store; currently the player-links table,
    /// and the crate's general-purpose database going forward.
    pub store_database_url: String,
    /// Configured cadence for internal background refreshes; `None` disables it.
    pub refresh_interval: Option<Duration>,
}

impl Default for NflDataConfig {
    fn default() -> Self {
        Self {
            fanduel_database_url: "sqlite://./storage/fanduel-data.db".to_owned(),
            store_database_url: "sqlite://./storage/nfl-data.db".to_owned(),
            refresh_interval: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_documented_storage_layout() {
        let config = NflDataConfig::default();
        assert_eq!(
            config.fanduel_database_url,
            "sqlite://./storage/fanduel-data.db"
        );
        assert_eq!(config.store_database_url, "sqlite://./storage/nfl-data.db");
        assert_eq!(config.refresh_interval, None);
    }
}
