/// Errors returned while parsing FanDuel salary CSV files or accessing salary data.
#[derive(Debug, thiserror::Error)]
pub enum FanduelDataError {
    /// The CSV could not be read or deserialized.
    #[error("CSV error: {0}")]
    Csv(#[from] csv::Error),
    /// A SQLite operation failed.
    #[error("database error: {0}")]
    Sqlx(#[from] sqlx::Error),
    /// Applying database migrations failed.
    #[error("migration error: {0}")]
    Migration(#[from] sqlx::migrate::MigrateError),
    /// A CSV row contains invalid or unsupported FanDuel data.
    #[error("invalid FanDuel CSV row: {0}")]
    InvalidRow(String),
    /// A game matchup could not be parsed.
    #[error("invalid matchup: {0}")]
    MatchupParseError(String),
}
