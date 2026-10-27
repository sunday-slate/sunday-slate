mod assets;
mod error;
mod nflverse;
mod view;

use std::sync::Arc;

use axum::Router;
use time::OffsetDateTime;

use crate::{NflData, Season};

/// Configuration for the admin router: which season it manages and an optional
/// clock override for tests.
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

/// Build NFL-data's self-contained administration router.
///
/// The returned router is not independently authenticated. The host must apply
/// its existing session and global-admin authorization before mounting it.
pub fn router<S>(nfl: Arc<NflData>, config: AdminConfig) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    Router::new()
        .nest("/salary-imports", salary_imports::router())
        .route("/", axum::routing::get(view::index))
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
    // The standalone admin test binaries live in this directory but are
    // compiled separately via `[[test]]` entries (see Cargo.toml); only the
    // shared fixture module is part of the crate's test build.
    pub(crate) mod salary_fixture;
}
