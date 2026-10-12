#[derive(Debug, thiserror::Error)]
pub enum FanduelDataError {
    #[error("CSV error: {0}")]
    Csv(#[from] csv::Error),
    #[error("database error: {0}")]
    Sqlx(#[from] sqlx::Error),
    #[error("migration error: {0}")]
    Migration(#[from] sqlx::migrate::MigrateError),
    #[error("invalid FanDuel CSV row: {0}")]
    InvalidRow(String),
    #[error("invalid matchup: {0}")]
    MatchupParseError(String),
}
