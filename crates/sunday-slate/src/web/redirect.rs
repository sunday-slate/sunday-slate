use super::is_htmx;
use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::response::{IntoResponse, Redirect, Response};
use axum_htmx::HxRedirect;

pub fn redirect(htmx: bool, location: impl Into<String>) -> Response {
    let location = location.into();
    if htmx {
        (HxRedirect(location), StatusCode::OK).into_response()
    } else {
        Redirect::to(&location).into_response()
    }
}

pub fn redirect_parts(parts: &Parts, location: &str) -> Response {
    redirect(is_htmx(&parts.headers), location)
}
