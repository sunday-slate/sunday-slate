use std::sync::Arc;

use axum::{
    extract::State,
    response::{IntoResponse, Response},
};
use axum_htmx::HxRequest;

use crate::{NflData, admin::error::AdminError};

use super::view::NflverseTemplate;

pub(in crate::admin) async fn show(
    HxRequest(is_htmx): HxRequest,
    State(nfl): State<Arc<NflData>>,
) -> Result<Response, AdminError> {
    render(is_htmx, &nfl).await
}

pub(in crate::admin) async fn start(
    HxRequest(is_htmx): HxRequest,
    State(nfl): State<Arc<NflData>>,
) -> Result<Response, AdminError> {
    nfl.provider.request_refresh().await?;
    render(is_htmx, &nfl).await
}

async fn render(is_htmx: bool, nfl: &NflData) -> Result<Response, AdminError> {
    let template = NflverseTemplate::load(nfl).await?;
    let html = if is_htmx {
        askama::Template::render(&template.as_panel())?
    } else {
        askama::Template::render(&template)?
    };
    Ok(axum::response::Html(html).into_response())
}
