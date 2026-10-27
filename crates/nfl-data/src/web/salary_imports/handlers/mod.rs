mod default_week;
mod index;
mod player_search;
mod players;
mod resolve;
mod upload;
mod week;

pub(crate) use default_week::default_season_week;

use crate::web::AdminState;
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

pub fn router() -> Router<AdminState> {
    Router::new()
        .route("/", get(index).post(upload))
        .route("/{week}", get(week))
        .route("/{week}/players/{fd_player_id}", get(players))
        .route("/{week}/players/{fd_player_id}/search", get(player_search))
        .route("/{week}/players/{fd_player_id}/resolve", post(resolve))
}
