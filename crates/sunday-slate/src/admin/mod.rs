//! Admin tools, gated as a subtree.
//!
//! Every route under `/admin` sits behind [`require_admin`], so the individual
//! handlers do not repeat the check. New admin features nest their own router
//! here rather than wiring their own auth.

pub mod avatars;
pub mod banner;
pub mod contests;
pub mod salary_imports;

use askama::Template;
use axum::Router;
use axum::extract::Request;
use axum::middleware::Next;
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;

use crate::auth::AdminUser;
use crate::chrome::Chrome;
use crate::{AppError, AppState};

#[derive(Template)]
#[template(path = "admin/index.html")]
struct IndexTemplate {
    chrome: Chrome,
}

async fn index() -> Result<impl IntoResponse, AppError> {
    Ok(Html(
        IndexTemplate {
            chrome: Chrome::unlit("Admin Tools"),
        }
        .render()?,
    ))
}

/// Whether `path` is in the admin subtree — the landing page or anything
/// nested under it. The one place that rule is written down.
pub(crate) fn is_admin_path(path: &str) -> bool {
    ["/admin", "/nfl-data-admin"]
        .iter()
        .any(|root| path == *root || path.starts_with(&format!("{root}/")))
}

/// Reject anyone but a signed-in admin before an admin handler runs. Extracting
/// [`AdminUser`] here returns its rejection — 403 for a non-admin, the login
/// redirect for an anonymous request — so the subtree needs no per-handler gate.
async fn require_admin(_: AdminUser, req: Request, next: Next) -> Response {
    next.run(req).await
}

/// The admin subtree: the landing page plus every feature router nested under
/// it, all behind [`require_admin`].
pub fn router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/admin", get(index))
        .nest("/admin/avatars", avatars::router())
        .nest("/admin/contests", contests::router())
        .nest("/admin/salary-imports", salary_imports::router())
        .route_layer(axum::middleware::from_fn_with_state(state, require_admin))
}

/// Mount NFL-data administration behind the host's existing global-admin gate.
pub(crate) fn nfl_data_router(state: AppState) -> Router<AppState> {
    Router::new()
        .nest(
            "/nfl-data-admin",
            nfl_data::admin_router::<AppState>(std::sync::Arc::clone(&state.nfl)),
        )
        .route_layer(axum::middleware::from_fn_with_state(state, require_admin))
}

#[cfg(test)]
mod tests {
    use crate::tests::TestApp;
    use crate::tests::factories::{self, UserOptions};
    use http::StatusCode;

    #[tokio::test]
    async fn admin_sees_the_tools_landing_page() {
        let app = TestApp::new().await;
        let admin = factories::user(
            &app.pool,
            UserOptions {
                is_admin: true,
                ..Default::default()
            },
        )
        .await;
        app.login_as(&admin.user).await;
        let page = app.get("/admin").await;
        page.assert_status_ok();
        page.assert_text_contains("Admin Tools");
        page.assert_text_contains("/admin/salary-imports");
        page.assert_text_contains("/admin/avatars");
    }

    #[tokio::test]
    async fn non_admin_cannot_open_admin() {
        let app = TestApp::new().await;
        let user = factories::user(
            &app.pool,
            UserOptions {
                is_admin: false,
                ..Default::default()
            },
        )
        .await;
        app.login_as(&user.user).await;
        app.get("/admin").await.assert_status(StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn anonymous_is_redirected_from_admin() {
        let app = TestApp::new().await;
        let resp = app.get("/admin").await;
        assert!(
            resp.status_code().is_redirection(),
            "expected a redirect, got {}",
            resp.status_code()
        );
        assert!(
            resp.header("location")
                .to_str()
                .unwrap()
                .starts_with("/login")
        );
    }
}
