use fanduel_data::FanduelDataError;
use nflverse_data::NflverseDataError;

/// Common error for nflverse, FanDuel, slate, and local-store operations.
#[derive(Debug, thiserror::Error)]
pub enum NflDataError {
    #[error("nflverse failed: {0}")]
    Nflverse(#[from] NflverseDataError),
    #[error("FanDuel failed: {0}")]
    Fanduel(#[from] FanduelDataError),
    #[error("salary matchup does not exist in the requested season week")]
    Schedule,
    #[error("NFL data store database operation failed: {0}")]
    Database(#[from] sqlx::Error),
    #[error("NFL data store migration failed: {0}")]
    Migration(#[from] sqlx::migrate::MigrateError),
    #[error("Tank01 configuration failed: {0}")]
    Tank01Configuration(#[from] tank01_data::Tank01ClientError),
}
