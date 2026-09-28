mod extract;
mod handlers;
mod model;
mod resolve;
pub mod store;

use crate::AppError;
use crate::AppState;
use axum::Router;
use axum::routing::{get, post};
pub use extract::{CurrentLeague, MaybeLeague};
pub use model::League;
pub use resolve::resolve;
pub(crate) use resolve::{LeagueSwitched, set_active};
use tower_sessions::Session;

/// Session key holding a league the admin just created and has no team in
/// yet. Carries the create-league bounce to /teams/new without league
/// resolution admitting a team-less league.
const PENDING_CREATION_KEY: &str = "pending_creation_league_id";

pub(crate) async fn set_pending_creation(
    session: &Session,
    league_id: i64,
) -> Result<(), AppError> {
    session.insert(PENDING_CREATION_KEY, league_id).await?;
    Ok(())
}

/// The just-created league id, if any. Best-effort — a session error reads
/// as none.
pub(crate) async fn pending_creation(session: &Session) -> Option<i64> {
    session.get(PENDING_CREATION_KEY).await.ok().flatten()
}

pub(crate) async fn clear_pending_creation(session: &Session) -> Result<(), AppError> {
    session.remove::<i64>(PENDING_CREATION_KEY).await?;
    Ok(())
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/leagues/new", get(handlers::new))
        .route("/leagues", post(handlers::create))
        .route("/leagues/switch", post(handlers::switch))
}
