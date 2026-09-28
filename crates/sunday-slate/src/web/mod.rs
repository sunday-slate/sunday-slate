mod form;
mod redirect;

pub use form::{FormErrors, FormInput, FormView, Valid, form_response};
pub use redirect::{redirect, redirect_parts};

use axum::http::HeaderMap;

/// True when the request was issued by htmx (carries the `HX-Request` header).
pub(crate) fn is_htmx(headers: &HeaderMap) -> bool {
    headers.contains_key("HX-Request")
}
