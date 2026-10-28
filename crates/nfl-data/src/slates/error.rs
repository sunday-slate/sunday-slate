use nflverse_data::NflverseDataError as NflDataError;

/// Errors surfaced by slate lookups.
#[derive(Debug, thiserror::Error)]
pub enum SlateError {
    #[error("nfl data error: {0}")]
    Nfl(#[from] NflDataError),
    #[error("slate database error: {0}")]
    Database(#[from] sqlx::Error),
}
