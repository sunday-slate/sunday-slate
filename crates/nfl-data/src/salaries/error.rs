use fanduel_data::FanduelDataError;
use nflverse_data::NflverseDataError as NflDataError;

/// Errors from the salary domain: source parsing, schedule resolution, and
/// the NFL data store.
#[derive(Debug, thiserror::Error)]
pub enum SalaryError {
    #[error("FanDuel salary import failed: {0}")]
    Fanduel(#[from] FanduelDataError),
    #[error("NFL schedule lookup failed: {0}")]
    Nfl(#[from] NflDataError),
    #[error("salary matchup does not exist in the requested season week")]
    Schedule,
    #[error("NFL data store database operation failed: {0}")]
    Database(#[from] sqlx::Error),
    #[error("NFL data store migration failed: {0}")]
    Migration(#[from] sqlx::migrate::MigrateError),
}
