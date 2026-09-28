use askama::Template;
use axum::{
    Router,
    extract::{Path, State},
    http::StatusCode,
    response::{Html, IntoResponse, Redirect, Response},
    routing::get,
};
use axum_htmx::{HxRedirect, HxRequest};

use crate::{AppError, AppState};

/// Dev inbox router. Mounted only when the mailer is Capture.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/_dev/mail", get(index).post(clear))
        .route("/_dev/mail/{idx}", get(detail))
}

struct MailIndexItem {
    idx: usize,
    to: String,
    subject: String,
}

#[derive(Template)]
#[template(path = "dev/mail_index.html")]
struct MailIndexPage {
    emails: Vec<MailIndexItem>,
}

async fn index(State(state): State<AppState>) -> Result<Html<String>, AppError> {
    let emails = state.mailer.all();
    let items: Vec<MailIndexItem> = emails
        .into_iter()
        .rev()
        .enumerate()
        .map(|(idx, e)| MailIndexItem {
            idx,
            to: e.to,
            subject: e.subject,
        })
        .collect();

    let page = MailIndexPage { emails: items };
    Ok(Html(page.render()?))
}

#[derive(Template)]
#[template(path = "dev/mail_detail.html")]
struct MailDetailPage {
    from: String,
    to: String,
    subject: String,
    html: String,
    text: String,
}

async fn detail(
    State(state): State<AppState>,
    Path(idx): Path<usize>,
) -> Result<Html<String>, AppError> {
    let email = state.mailer.get(idx).ok_or(AppError::NotFound)?;

    let page = MailDetailPage {
        from: email.from,
        to: email.to,
        subject: email.subject,
        html: email.html,
        text: email.text,
    };
    Ok(Html(page.render()?))
}

async fn clear(
    HxRequest(htmx): HxRequest,
    State(state): State<AppState>,
) -> Result<Response, AppError> {
    state.mailer.clear();
    if htmx {
        Ok((HxRedirect("/_dev/mail".to_string()), StatusCode::OK).into_response())
    } else {
        Ok(Redirect::to("/_dev/mail").into_response())
    }
}
