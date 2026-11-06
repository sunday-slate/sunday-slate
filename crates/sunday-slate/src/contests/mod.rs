mod handlers;
pub mod model;
pub mod service;
pub mod store;

pub use model::{Contest, ContestId};

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
