pub mod service;
// The aggregation module keeps the domain noun (rows, ranks, counts) under
// the same namespace the page uses — `standings::standings` — rather than
// inventing a second word for it.
#[allow(clippy::module_inception)]
pub mod standings;

mod handlers;

use crate::AppState;
use axum::Router;
use axum::routing::get;

pub fn router() -> Router<AppState> {
    Router::new().route("/standings", get(handlers::index))
}
