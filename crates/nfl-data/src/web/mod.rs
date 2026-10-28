mod assets;
mod error;
mod nflverse;
mod slates;
mod view;

use std::sync::Arc;

use axum::Router;
use time::OffsetDateTime;

use crate::{NflData, Season};

/// Admin router config: season and optional test clock override.
#[derive(Debug, Clone)]
pub struct AdminConfig {
    pub season: Season,
    pub now_override: Option<OffsetDateTime>,
}

/// Private state shared by the admin handlers.
#[derive(Clone)]
pub(crate) struct AdminState {
    nfl: Arc<NflData>,
    config: AdminConfig,
}

impl axum::extract::FromRef<AdminState> for Arc<NflData> {
    fn from_ref(state: &AdminState) -> Self {
        Arc::clone(&state.nfl)
    }
}

/// Build the admin router. Not authenticated: the host must mount it behind
/// its own admin gate.
pub fn router<S>(nfl: Arc<NflData>, config: AdminConfig) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    Router::new()
        .nest("/salary-imports", salary_imports::router())
        .route("/", axum::routing::get(view::index))
        .route("/slates", axum::routing::get(slates::index))
        .route("/slates/{id}", axum::routing::get(slates::show))
        .route(
            "/nflverse",
            axum::routing::get(nflverse::show).post(nflverse::start),
        )
        .route(
            "/static/{*path}",
            axum::routing::get(assets::serve).head(assets::serve),
        )
        .with_state(AdminState { nfl, config })
}

mod salary_imports;

#[cfg(test)]
pub(crate) mod tests {
    // Shared fixture; the standalone [[test]] binaries compile separately.
    pub(crate) mod salary_fixture;
    pub(crate) mod salary_imports;
}
