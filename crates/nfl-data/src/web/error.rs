use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
};
use thiserror::Error;

#[derive(Debug, Error)]
pub(super) enum AdminError {
    #[error(transparent)]
    Provider(#[from] crate::NflDataError),
    #[error(transparent)]
    Render(#[from] askama::Error),
}

impl IntoResponse for AdminError {
    fn into_response(self) -> Response {
        tracing::error!(error = %self, "NFL data admin request failed");
        (StatusCode::INTERNAL_SERVER_ERROR, "Internal server error").into_response()
    }
}

#[cfg(test)]
mod tests {
    use axum::{body::to_bytes, http::StatusCode, response::IntoResponse};

    use super::AdminError;

    #[tokio::test]
    async fn render_errors_return_a_generic_server_error() {
        let response = AdminError::Render(askama::Error::custom("private detail")).into_response();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert_eq!(body, "Internal server error");
    }
}
