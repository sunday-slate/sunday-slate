mod assets;
mod error;
mod nflverse;
mod view;

use std::sync::Arc;

use axum::Router;

use crate::NflData;

/// Build NFL-data's self-contained administration router.
///
/// The returned router is not independently authenticated. The host must apply
/// its existing session and global-admin authorization before mounting it.
pub fn router<S>(nfl: Arc<NflData>) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/", axum::routing::get(view::index))
        .route(
            "/nflverse",
            axum::routing::get(nflverse::show).post(nflverse::start),
        )
        .route(
            "/static/{*path}",
            axum::routing::get(assets::serve).head(assets::serve),
        )
        .with_state(nfl)
}
