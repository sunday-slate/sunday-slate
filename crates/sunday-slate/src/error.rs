//! Application-wide error type. All handlers return `Result<T, AppError>`.
//!
//! Library errors convert via `?` with `#[from]` derives. Status code mapping
//! and logging happen in `IntoResponse`.

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
};

#[derive(thiserror::Error, Debug)]
pub enum AppError {
    #[error("not found")]
    NotFound,

    #[error("unauthorized")]
    Unauthorized,

    #[error("forbidden")]
    Forbidden,

    #[error("bad request: {0}")]
    BadRequest(String),

    #[error("payload too large: {0}")]
    PayloadTooLarge(String),

    #[error("internal server error: {0}")]
    Internal(String),

    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),

    #[error(transparent)]
    Session(#[from] tower_sessions::session::Error),

    #[error(transparent)]
    Template(#[from] askama::Error),

    #[error(transparent)]
    Mail(#[from] crate::mail::MailError),

    #[error(transparent)]
    Entries(#[from] crate::entries::EntriesError),

    #[error(transparent)]
    NflData(#[from] nfl_data::NflDataError),

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl From<axum_login::Error<crate::Db>> for AppError {
    fn from(e: axum_login::Error<crate::Db>) -> Self {
        match e {
            axum_login::Error::Session(err) => Self::Session(err),
            axum_login::Error::Backend(err) => err,
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = match &self {
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::Forbidden => StatusCode::FORBIDDEN,
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::PayloadTooLarge(_) => StatusCode::PAYLOAD_TOO_LARGE,
            Self::Sqlx(sqlx::Error::RowNotFound) => StatusCode::NOT_FOUND,
            Self::Sqlx(_)
            | Self::Session(_)
            | Self::Template(_)
            | Self::Mail(_)
            | Self::Entries(_)
            | Self::Internal(_)
            | Self::NflData(_)
            | Self::Other(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };

        if status.is_server_error() {
            tracing::error!(error = ?self, status = %status, "request failed");
        } else {
            tracing::debug!(error = ?self, status = %status, "request rejected");
        }

        (status, self.to_string()).into_response()
    }
}
