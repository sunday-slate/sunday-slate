use nflverse_data::NflverseDataError as NflDataError;

/// Errors surfaced by slate lookups: provider data failures or slate storage
/// failures, each preserving its source.
#[derive(Debug, thiserror::Error)]
pub enum SlateError {
    /// An nfl-data provider error, propagated unchanged.
    #[error("nfl data error: {0}")]
    Nfl(#[from] NflDataError),
    /// A slate storage operation failed.
    #[error("slate database error: {0}")]
    Database(#[from] sqlx::Error),
}
