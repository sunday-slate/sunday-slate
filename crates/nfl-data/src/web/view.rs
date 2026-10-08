use std::sync::Arc;

use askama::Template;
use axum::{extract::State, http::StatusCode, response::Html};

use crate::NflData;

#[derive(Template)]
#[template(path = "admin/index.html", config = "src/web/askama.toml")]
struct IndexTemplate;

pub(super) async fn index(
    State(_nfl): State<Arc<NflData>>,
) -> Result<Html<String>, (StatusCode, &'static str)> {
    IndexTemplate.render().map(Html).map_err(|error| {
        tracing::error!(%error, "failed to render NFL-data admin landing page");
        (StatusCode::INTERNAL_SERVER_ERROR, "Internal server error")
    })
}
