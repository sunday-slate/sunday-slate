#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FanduelDataConfig {
    pub database_url: String,
}

impl Default for FanduelDataConfig {
    fn default() -> Self {
        Self {
            database_url: "sqlite://./storage/fanduel-data.db".to_owned(),
        }
    }
}
