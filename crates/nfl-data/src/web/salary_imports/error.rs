use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
};
use thiserror::Error;

#[derive(Debug, Error)]
pub(crate) enum SalaryAdminError {
    #[error("not found")]
    NotFound,
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error(transparent)]
    Domain(#[from] crate::NflDataError),
    #[error(transparent)]
    Render(#[from] askama::Error),
}

impl IntoResponse for SalaryAdminError {
    fn into_response(self) -> Response {
        let status = match &self {
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        if status.is_server_error() {
            tracing::error!(error = ?self, status = %status, "salary admin request failed");
        } else {
            tracing::debug!(error = ?self, status = %status, "request rejected");
        }
        (status, self.to_string()).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn bad_request_maps_to_400_with_message() {
        let response = SalaryAdminError::BadRequest("invalid season".into()).into_response();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(body, "bad request: invalid season");
    }

    #[tokio::test]
    async fn not_found_maps_to_404() {
        let response = SalaryAdminError::NotFound.into_response();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn fanduel_error_maps_to_500() {
        let response = SalaryAdminError::from(crate::NflDataError::Fanduel(
            crate::FanduelDataError::InvalidRow("bad row".into()),
        ))
        .into_response();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }
}
