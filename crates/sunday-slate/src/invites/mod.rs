mod extract;
mod handlers;
pub mod model;
pub mod service;
pub mod store;

use crate::invites::model::InviteLink;
use crate::{AppError, AppState};
use axum::Router;
use axum::routing::{get, post};
use tower_sessions::Session;

/// Session key holding the invite id the visitor most recently accepted, so
/// team creation consumes that invite rather than the newest one for their
/// email.
const PENDING_INVITE_KEY: &str = "pending_invite_id";

/// Session key carrying a just-minted accept link across the redirect to the
/// invite list. A link cannot survive in the database — the secret is hashed —
/// and putting it in the URL would leak it into history and logs, so it rides
/// the session for exactly one render.
const FRESH_LINK_KEY: &str = "fresh_invite_link";

/// Remember the accepted invite for the coming team creation.
pub(crate) async fn set_pending(session: &Session, invite_id: i64) -> Result<(), AppError> {
    session.insert(PENDING_INVITE_KEY, invite_id).await?;
    Ok(())
}

/// The accepted invite id, if any. Best-effort — a session error reads as
/// none, and team creation falls back to the newest active invite.
pub(crate) async fn pending(session: &Session) -> Option<i64> {
    session.get(PENDING_INVITE_KEY).await.ok().flatten()
}

/// Hand a freshly minted link to the next render of the invite list.
pub(crate) async fn set_fresh_link(
    session: &Session,
    invite_id: i64,
    link: InviteLink,
) -> Result<(), AppError> {
    session.insert(FRESH_LINK_KEY, (invite_id, link)).await?;
    Ok(())
}

/// The link stashed by the last create-or-copy, clearing it as it goes so a
/// reload of the list does not keep showing it. Best-effort: a session error
/// reads as none, and the list simply renders without a link.
pub(crate) async fn take_fresh_link(session: &Session) -> Option<(i64, InviteLink)> {
    session.remove(FRESH_LINK_KEY).await.ok().flatten()
}

/// Commissioner-only routes; mount under `login_required`.
pub fn commissioner_router() -> Router<AppState> {
    Router::new()
        .route(
            "/invites",
            get(handlers::index).post(handlers::handle_create),
        )
        .route("/invites/new", get(handlers::new))
        .route("/invites/{id}/link", post(handlers::link))
        .route("/invites/{id}/revoke", post(handlers::revoke))
}

/// Public visitor routes; mount at session level (outside `login_required`).
pub fn public_router() -> Router<AppState> {
    Router::new()
        .route("/invite", get(handlers::show))
        .route("/invite/accept", get(handlers::accept))
        .route(
            "/invite/create-user",
            get(handlers::create_user).post(handlers::handle_create_user),
        )
}
