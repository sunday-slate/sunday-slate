mod handlers;
pub mod service;

use crate::AppState;
use axum::Router;
use axum::routing::get;

/// Member-facing contest pages: the season list and one contest's details.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/contests", get(handlers::index))
        .route("/contests/{id}", get(handlers::show))
        .route("/contests/{id}/events", get(handlers::events))
}
