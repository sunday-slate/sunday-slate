use fanduel_data::FanduelDataError;
use nflverse_data::NflverseDataError as NflDataError;

/// Errors from the salary reconciliation domain: source parsing, schedule
/// resolution, and the reconciliation database.
#[derive(Debug, thiserror::Error)]
pub enum SalaryError {
    #[error("FanDuel salary import failed: {0}")]
    Fanduel(#[from] FanduelDataError),
    #[error("NFL schedule lookup failed: {0}")]
    Nfl(#[from] NflDataError),
    #[error("salary matchup does not exist in the requested season week")]
    Schedule,
    #[error("NFL reconciliation database operation failed: {0}")]
    Database(#[from] sqlx::Error),
    #[error("NFL reconciliation migration failed: {0}")]
    Migration(#[from] sqlx::migrate::MigrateError),
}
