use axum::Router;
use axum::extract::State;
use axum::response::Response;
use axum::routing::{get, post};
use axum_htmx::HxRequest;

use crate::admin::avatars::view::AvatarsTemplate;
use crate::avatars::start_fetch;
use crate::web::form_response;
use crate::{AppError, AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(index))
        .route("/fetch", post(fetch))
}

async fn index(
    HxRequest(is_htmx): HxRequest,
    State(state): State<AppState>,
) -> Result<Response, AppError> {
    form_response(is_htmx, &AvatarsTemplate::load(&state).await?)
}

async fn fetch(hx: HxRequest, State(state): State<AppState>) -> Result<Response, AppError> {
    start_fetch(&state);
    index(hx, State(state)).await
}

#[cfg(test)]
mod tests {

    use crate::media::store as media_store;
    use crate::tests::TestApp;
    use crate::tests::factories::{self, UserOptions};
    use http::StatusCode;

    async fn link_all_teams(app: &TestApp) {
        let media_id = media_store::insert(&app.pool, "admin-dummy.png", "image/png", 1, 1, 1)
            .await
            .expect("dummy media");
        sqlx::query("UPDATE nfl_teams SET logo_media_id = ?1")
            .bind(media_id)
            .execute(&app.pool)
            .await
            .expect("link teams");
    }

    async fn admin_app() -> TestApp {
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
        app
    }

    #[tokio::test]
    async fn admin_avatar_index_shows_missing_counts() {
        let app = admin_app().await;
        let page = app.get("/admin/avatars").await;
        page.assert_status_ok();
        page.assert_text_contains("Players missing headshots");
        page.assert_text_contains("Teams missing logos");
        let body = page.text();
        let team_count = body
            .split("Teams missing logos</dt>")
            .nth(1)
            .expect("team count");
        assert!(team_count.contains(">32</dd>"));
        assert!(body.contains(r#"method="post" action="/admin/avatars/fetch""#));
        assert!(body.contains("Fetch missing images"));
    }

    #[tokio::test]
    async fn admin_avatar_fetch_starts_a_background_run_and_reports_it() {
        let app = admin_app().await;
        link_all_teams(&app).await;

        let response = app.post_htmx("/admin/avatars/fetch", "").await;
        response.assert_status_ok();
        let body = response.text();
        assert!(body.contains(r#"id="avatars-panel""#));
        assert!(!body.contains("<!doctype html>"));
        // Every team already has a logo and player discovery finds nothing, so
        // the run is empty and finishes instantly: the response may already
        // show the completed panel. Accept either the running panel's poll
        // trigger or the completed panel's summary text.
        assert!(body.contains(r#"hx-trigger="every 3s""#) || body.contains("Last fetch completed"));

        app.state.avatar_runner.wait_idle().await;
        let page = app.get("/admin/avatars").await;
        page.assert_status_ok();
        let body = page.text();
        assert!(body.contains("Last fetch completed"));
        assert!(body.contains("Fetched 0 headshots and 0 logos. 0 failed, 0 have no source."));
    }

    #[tokio::test]
    async fn admin_avatar_routes_forbid_non_admin() {
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
        app.get("/admin/avatars")
            .await
            .assert_status(StatusCode::FORBIDDEN);
        app.post("/admin/avatars/fetch", "")
            .await
            .assert_status(StatusCode::FORBIDDEN);
    }
}
