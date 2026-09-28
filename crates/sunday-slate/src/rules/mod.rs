//! The league's rules: scoring values, roster construction, and how contests
//! and the season standings work. Static content — no state, no queries.

mod handlers;

use crate::AppState;
use axum::Router;
use axum::routing::get;

pub fn router() -> Router<AppState> {
    Router::new().route("/rules", get(handlers::index))
}
