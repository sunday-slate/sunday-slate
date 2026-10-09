#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct SalaryError(pub(super) SalaryErrorKind);

#[derive(Debug, thiserror::Error)]
pub(super) enum SalaryErrorKind {
    #[error(transparent)]
    Sql(#[from] sqlx::Error),
    #[error(transparent)]
    Migration(#[from] sqlx::migrate::MigrateError),
    #[error(
        "legacy salary migration found pending salary reviews; resolve or discard them before upgrading"
    )]
    PendingReviews,
    #[error("legacy salary migration conflicts with managed salary data: {0}")]
    Conflict(String),
    #[error("managed database contains an unrelated or unsupported schema")]
    UnsupportedSchema,
}

impl From<sqlx::Error> for SalaryError {
    fn from(error: sqlx::Error) -> Self {
        Self(SalaryErrorKind::Sql(error))
    }
}

impl From<sqlx::migrate::MigrateError> for SalaryError {
    fn from(error: sqlx::migrate::MigrateError) -> Self {
        Self(SalaryErrorKind::Migration(error))
    }
}

impl SalaryError {
    pub(super) fn conflict(message: impl Into<String>) -> Self {
        Self(SalaryErrorKind::Conflict(message.into()))
    }

    pub(super) fn pending_reviews() -> Self {
        Self(SalaryErrorKind::PendingReviews)
    }

    pub(super) fn unsupported_schema() -> Self {
        Self(SalaryErrorKind::UnsupportedSchema)
    }
}
