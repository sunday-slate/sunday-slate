pub mod contest_entries;
mod extract;
mod handlers;
mod logo;
mod model;
pub mod open_contests;
pub mod store;

use crate::AppState;
use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::routing::{get, post};

pub use extract::{CurrentTeam, MaybeTeam, NoTeam, OwnedTeam, TeamCreator, team_creation_target};
pub use model::{FantasyTeam, FantasyTeamId, LeagueTeam, monogram};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/teams/new", get(handlers::new))
        .route("/teams", post(handlers::create))
        .route("/teams/{id}", get(handlers::show))
        .route(
            "/teams/{id}/edit",
            get(handlers::edit_names).post(handlers::update_names),
        )
        .route(
            "/teams/{id}/logo",
            get(handlers::logo_editor)
                .post(handlers::update_logo)
                .layer(DefaultBodyLimit::max(5 * 1024 * 1024)),
        )
}
