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
        let player_count = body
            .split("Players missing headshots</dt>")
            .nth(1)
            .expect("player count");
        assert!(player_count.contains(">0</dd>"));
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
        crate::nfl_players::store::upsert(&app.pool, "00-NO-SOURCE", "no-source")
            .await
            .expect("crosswalk");
        app.nfl
            .seed_for_test(
                &[nfl_data::Player {
                    gsis_id: "00-NO-SOURCE".into(),
                    espn_id: None,
                    full_name: "No Source".into(),
                    first_name: None,
                    last_name: None,
                    position: Some("QB".into()),
                    latest_team: Some(nfl_data::TeamAbbr("KC".into())),
                    status: None,
                    birth_date: None,
                    headshot_url: None,
                }],
                &[],
            )
            .await
            .expect("identity");

        let response = app.post_htmx("/admin/avatars/fetch", "").await;
        response.assert_status_ok();
        let body = response.text();
        assert!(body.contains(r#"id="avatars-panel""#));
        assert!(!body.contains("<!doctype html>"));
        // The only item here is a no-source skip, which finishes instantly,
        // so the run may already be idle by the time this response renders.
        // Accept either the running panel's poll trigger or the completed
        // panel's summary text.
        assert!(body.contains(r#"hx-trigger="every 3s""#) || body.contains("Last fetch completed"));

        app.state.avatar_runner.wait_idle().await;
        let page = app.get("/admin/avatars").await;
        page.assert_status_ok();
        let body = page.text();
        assert!(body.contains("Last fetch completed"));
        assert!(body.contains("Fetched 0 headshots and 0 logos. 0 failed, 1 have no source."));
        let player_count = body
            .split("Players missing headshots</dt>")
            .nth(1)
            .expect("player count");
        assert!(player_count.contains(">1</dd>"));
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
