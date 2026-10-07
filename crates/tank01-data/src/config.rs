use std::time::Duration;

use utils::Secret;

const DEFAULT_API_BASE_URL: &str =
    "https://tank01-nfl-live-in-game-real-time-statistics-nfl.p.rapidapi.com";
const DEFAULT_RAPIDAPI_HOST: &str =
    "tank01-nfl-live-in-game-real-time-statistics-nfl.p.rapidapi.com";

#[derive(Clone)]
pub struct Tank01Config {
    pub api_key: Secret,
    pub api_base_url: String,
    pub rapidapi_host: String,
    pub timeout: Duration,
}

impl std::fmt::Debug for Tank01Config {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Tank01Config")
            .field("api_key", &self.api_key)
            .field("api_base_url", &self.api_base_url)
            .field("rapidapi_host", &self.rapidapi_host)
            .field("timeout", &self.timeout)
            .finish()
    }
}

impl Tank01Config {
    pub fn new(api_key: Secret) -> Self {
        Self {
            api_key,
            api_base_url: DEFAULT_API_BASE_URL.into(),
            rapidapi_host: DEFAULT_RAPIDAPI_HOST.into(),
            timeout: Duration::from_secs(20),
        }
    }

    pub fn with_base_url(mut self, api_base_url: impl Into<String>) -> Self {
        self.api_base_url = api_base_url.into();
        self
    }

    pub fn with_api_base_url(self, api_base_url: impl Into<String>) -> Self {
        self.with_base_url(api_base_url)
    }

    pub fn with_rapidapi_host(mut self, rapidapi_host: impl Into<String>) -> Self {
        self.rapidapi_host = rapidapi_host.into();
        self
    }

    pub fn with_host(self, rapidapi_host: impl Into<String>) -> Self {
        self.with_rapidapi_host(rapidapi_host)
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
}
