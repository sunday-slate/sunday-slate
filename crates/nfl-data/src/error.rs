use fanduel_data::FanduelDataError;
use nflverse_data::NflverseDataError;

/// Common error for NFL data operations across providers, slates, and the
/// local store.
#[derive(Debug, thiserror::Error)]
pub enum NflDataError {
    #[error("nflverse provider failed: {0}")]
    Nflverse(#[from] NflverseDataError),
    #[error("FanDuel provider failed: {0}")]
    Fanduel(#[from] FanduelDataError),
    #[error("salary matchup does not exist in the requested season week")]
    Schedule,
    #[error("NFL data store database operation failed: {0}")]
    Database(#[from] sqlx::Error),
    #[error("NFL data store migration failed: {0}")]
    Migration(#[from] sqlx::migrate::MigrateError),
}
