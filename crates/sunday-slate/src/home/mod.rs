//! The home dispatcher: `GET /` redirects to the page that represents the
//! league's current state — usually the current contest.

pub(crate) mod service;

mod handlers;

use crate::AppState;
use axum::Router;
use axum::routing::get;

pub fn router() -> Router<AppState> {
    Router::new().route("/", get(handlers::index))
}
