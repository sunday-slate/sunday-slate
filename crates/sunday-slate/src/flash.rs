//! Flash messages rendered as daisyUI toast alerts in `_flash.html`.
//!
//! Handlers push messages via [`axum_messages::Messages`]; the next page's
//! `_flash.html` partial drains them at render time via [`crate::context::flashes`]
//! and they're cleared from the session on response write — one-shot, never in the URL.

use askama::Template;
use axum::body::Body;
use axum::extract::{FromRequestParts, Request};
use axum::http::HeaderValue;
use axum::http::header::{CONTENT_LENGTH, CONTENT_TYPE};
use axum::middleware::Next;
use axum::response::Response;
use axum_messages::{Level, Message, Messages};

/// One message for `_flash.html`.
#[derive(Debug, Clone)]
pub struct Flash {
    pub level: &'static str,
    pub icon: &'static str,
    pub dismiss_ms: Option<u32>,
    pub text: String,
}

/// Drain pending messages into template-ready flashes.
pub fn drain(messages: Messages) -> Vec<Flash> {
    messages.map(Flash::from).collect()
}

impl Flash {
    fn from_level(level: Level, text: String) -> Self {
        let (level, icon, dismiss_ms) = style(level);
        Self {
            level,
            icon,
            dismiss_ms,
            text,
        }
    }

    /// Build a success flash directly, bypassing the session message store.
    /// Used for one-off htmx toast fragments where there's no redirect.
    pub fn success(text: impl Into<String>) -> Self {
        Self::from_level(Level::Success, text.into())
    }

    /// Build an info flash directly, bypassing the session message store.
    pub fn info(text: impl Into<String>) -> Self {
        Self::from_level(Level::Info, text.into())
    }
}

/// Renders pending flashes as an htmx out-of-band swap appended to `#flash`.
/// Used by [`htmx_flash`] for in-place htmx responses that never re-render the
/// layout. The per-alert markup mirrors `layout.html` by design.
#[derive(Template)]
#[template(path = "flash_oob.html")]
pub struct FlashOob {
    pub flashes: Vec<Flash>,
}

impl From<Message> for Flash {
    fn from(message: Message) -> Self {
        Self::from_level(message.level, message.message)
    }
}

/// daisyUI alert class, Phosphor icon, and auto-dismiss delay per message level.
///
/// Success is sticky (no auto-dismiss): these flashes carry instructions acted
/// on after reading ("check your email"), making auto-hide unrecoverable.
fn style(level: Level) -> (&'static str, &'static str, Option<u32>) {
    match level {
        Level::Debug => ("info", "info", Some(4_000)),
        Level::Info => ("info", "info", Some(6_000)),
        Level::Success => ("success", "check-circle", None),
        Level::Warning => ("warning", "warning", None),
        Level::Error => ("error", "warning-circle", None),
    }
}

/// Append pending flash messages to in-place htmx responses as an OOB swap.
///
/// Mounted between `request_ctx` and `MessagesManagerLayer`. On a full-page
/// render the layout has already drained the messages by the time this runs, so
/// it finds none and returns the response untouched. On an in-place htmx swap
/// (htmx request, no `HX-Redirect`/`HX-Location`) it drains the pending messages
/// and appends `flash_oob.html` to the body, which htmx applies out-of-band.
pub async fn htmx_flash(req: Request, next: Next) -> Response {
    let (mut parts, body) = req.into_parts();
    let is_htmx = parts.headers.contains_key("hx-request");
    let messages = Messages::from_request_parts(&mut parts, &()).await.ok();
    let res = next.run(Request::from_parts(parts, body)).await;

    if !is_htmx
        || res.headers().contains_key("hx-redirect")
        || res.headers().contains_key("hx-location")
    {
        return res;
    }

    let Some(messages) = messages else {
        return res;
    };
    let flashes = drain(messages.load());
    if flashes.is_empty() {
        return res;
    }

    let oob = match (FlashOob { flashes }).render() {
        Ok(html) => html,
        Err(err) => {
            tracing::error!(error = %err, "rendering flash OOB fragment");
            return res;
        }
    };

    let (mut parts, body) = res.into_parts();
    let existing = axum::body::to_bytes(body, usize::MAX)
        .await
        .unwrap_or_default();
    let mut combined = existing.to_vec();
    combined.extend_from_slice(oob.as_bytes());

    parts.headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    parts.headers.remove(CONTENT_LENGTH);
    Response::from_parts(parts, Body::from(combined))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every level must map to a non-empty daisyUI modifier and icon.
    #[test]
    fn every_level_has_a_style() {
        for level in [
            Level::Debug,
            Level::Info,
            Level::Success,
            Level::Warning,
            Level::Error,
        ] {
            let (class, icon, _) = style(level);
            assert!(!class.is_empty(), "{level:?} has an empty alert class");
            assert!(!icon.is_empty(), "{level:?} has an empty icon");
        }
    }

    #[test]
    fn info_flash_auto_dismisses() {
        let f = Flash::info("Now viewing Big League");
        assert_eq!(f.level, "info");
        assert!(f.dismiss_ms.is_some(), "info toasts auto-dismiss");
    }

    #[test]
    fn flash_oob_renders_oob_swap_with_message() {
        let html = FlashOob {
            flashes: vec![Flash::success("Saved.")],
        }
        .render()
        .expect("render oob");
        assert!(html.contains(r#"id="flash""#));
        assert!(html.contains(r#"hx-swap-oob="beforeend""#));
        assert!(html.contains("Saved."));
        assert!(html.contains("alert-success"));
    }

    #[test]
    fn flash_oob_empty_renders_container_only() {
        let html = FlashOob { flashes: vec![] }.render().expect("render empty");
        assert!(html.contains(r#"id="flash""#));
        assert!(!html.contains("alert-"));
    }
}
