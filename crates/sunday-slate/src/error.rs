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
            match &self {
                // Alternate Display keeps anyhow's source chain.
                Self::Other(error) => tracing::error!(
                    error = ?self,
                    source = %format!("{error:#}"),
                    status = %status,
                    "request failed"
                ),
                _ => tracing::error!(error = ?self, status = %status, "request failed"),
            }
            (status, "Internal server error").into_response()
        } else {
            tracing::debug!(error = ?self, status = %status, "request rejected");
            (status, self.to_string()).into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use axum::body::to_bytes;
    use axum::http::StatusCode;
    use axum::response::IntoResponse;

    use super::AppError;

    async fn body_of(error: AppError) -> (StatusCode, String) {
        let response = error.into_response();
        let status = response.status();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (status, String::from_utf8(body.to_vec()).unwrap())
    }

    #[tokio::test]
    async fn server_errors_hide_detail_behind_a_generic_body() {
        let (status, body) = body_of(AppError::Internal("SENTINEL_DETAIL".into())).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body, "Internal server error");
        assert!(!body.contains("SENTINEL_DETAIL"));

        let (status, body) = body_of(AppError::Other(anyhow::anyhow!("SENTINEL_CHAIN"))).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body, "Internal server error");
    }

    #[tokio::test]
    async fn client_errors_keep_their_messages_and_statuses() {
        let (status, body) = body_of(AppError::BadRequest("invalid season".into())).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body, "bad request: invalid season");

        let (status, body) = body_of(AppError::NotFound).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body, "not found");

        let (status, _) = body_of(AppError::Unauthorized).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);

        let (status, _) = body_of(AppError::Forbidden).await;
        assert_eq!(status, StatusCode::FORBIDDEN);

        let (status, _) = body_of(AppError::PayloadTooLarge("huge".into())).await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn missing_rows_stay_not_found() {
        // The special mapping keeps RowNotFound a 404.
        let (status, _) = body_of(AppError::Sqlx(sqlx::Error::RowNotFound)).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
}
