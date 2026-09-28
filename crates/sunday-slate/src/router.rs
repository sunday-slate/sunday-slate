use crate::assets::StaticAssetService;
use crate::mail::Mailer;
use crate::{
    AppState, Db, admin, auth, contests, draft_entry, entries, fantasy_teams, health, home,
    invites, leagues, mail, rules, sessions, standings,
};
use axum::Router;
use axum_login::{AuthManagerLayerBuilder, login_required};
use tower_http::services::ServeDir;

pub fn build(state: AppState, session_layer: sessions::Layer) -> Router {
    let auth_layer = AuthManagerLayerBuilder::new(state.db.clone(), session_layer).build();

    let protected = Router::new()
        .merge(home::router())
        .merge(standings::router())
        .merge(rules::router())
        .merge(contests::router())
        .merge(entries::router())
        .merge(draft_entry::router())
        .merge(leagues::router())
        .merge(fantasy_teams::router())
        .merge(invites::commissioner_router())
        .merge(admin::router(state.clone()))
        .route_layer(login_required!(Db, login_url = "/login"));

    let mut session_routes = protected
        .merge(auth::router())
        .merge(invites::public_router());

    // Dev-only capture mailbox
    if matches!(state.mailer, Mailer::Capture(_, _)) {
        session_routes = session_routes.merge(mail::capture::router());
    }

    // auth_layer must stay last in this list
    let session_routes = session_routes
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::context::request_ctx,
        ))
        .layer(axum::middleware::from_fn(crate::flash::htmx_flash))
        .layer(axum_messages::MessagesManagerLayer)
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth::bootstrap,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            leagues::resolve,
        ))
        .layer(auth_layer);

    let media_dir = state.config.media_dir.clone();

    // Static assets and the health probe, outside the session layer
    Router::new()
        .nest_service("/static", StaticAssetService)
        .nest_service("/media", ServeDir::new(media_dir))
        .merge(health::router())
        .merge(session_routes)
        .with_state(state)
}
