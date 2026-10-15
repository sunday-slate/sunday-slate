mod index;
mod player_search;
mod players;
mod resolve;
mod upload;
mod week;

#[cfg(test)]
mod tests;

use crate::AppState;
use axum::{
    Router,
    routing::{get, post},
};
use index::index;
use player_search::player_search;
use players::players;
use resolve::resolve;
use upload::upload;
use week::week;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(index).post(upload))
        .route("/{week}", get(week))
        .route("/{week}/players/{fd_player_id}", get(players))
        .route("/{week}/players/{fd_player_id}/search", get(player_search))
        .route("/{week}/players/{fd_player_id}/resolve", post(resolve))
}
