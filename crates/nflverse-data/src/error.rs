#[derive(Debug, thiserror::Error)]
pub enum NflDataError {
    #[error("http request failed: {0}")]
    Http(#[from] reqwest::Error),

    #[error("github api returned status {status} for {url}")]
    GitHubApi { status: u16, url: String },

    #[error("release {release_tag} has no usable asset named {name}")]
    MissingAsset { release_tag: String, name: String },

    #[error("failed to parse {asset} at row {row}: {source}")]
    Parse {
        asset: String,
        row: u64,
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    #[error("failed to decompress {asset}: {source}")]
    Gunzip {
        asset: String,
        source: std::io::Error,
    },

    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),

    #[error("migration error: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),
}
