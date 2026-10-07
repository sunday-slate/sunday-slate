use axum::Router;
use axum::extract::State;
use axum::response::Response;
use axum::routing::get;
use axum_htmx::HxRequest;

use crate::admin::nfl_sync::view::NflSyncTemplate;
use crate::web::form_response;
use crate::{AppError, AppState};

pub fn router() -> Router<AppState> {
    Router::new().route("/", get(show).post(start))
}

async fn show(
    HxRequest(is_htmx): HxRequest,
    State(state): State<AppState>,
) -> Result<Response, AppError> {
    form_response(is_htmx, &NflSyncTemplate::load(&state).await?)
}

async fn start(hx: HxRequest, State(state): State<AppState>) -> Result<Response, AppError> {
    state.nfl.request_refresh().await?;
    show(hx, State(state)).await
}

#[cfg(test)]
mod tests {
    use crate::tests::TestApp;
    use crate::tests::factories::{self, UserOptions};
    use http::StatusCode;

    #[tokio::test]
    async fn admin_reaches_nfl_sync_from_tools() {
        let app = TestApp::new().await;
        app.login_admin().await;

        let tools = app.get("/admin").await;
        tools.assert_status_ok();
        tools.assert_text_contains(r#"href="/admin/nfl-sync""#);

        let page = app.get("/admin/nfl-sync").await;
        page.assert_status_ok();
        page.assert_text_contains("NFL data sync");
        page.assert_text_contains("Sync now");
        let body = page.text();
        for dataset in [
            "schedules",
            "players",
            "weekly_rosters",
            "player_week_stats",
            "team_week_stats",
        ] {
            assert!(body.contains(&format!("<td>{dataset}</td>")), "{dataset}");
        }
        assert_eq!(body.matches("Never synced").count(), 5);
    }

    #[tokio::test]
    async fn non_admin_cannot_open_nfl_sync() {
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

        app.get("/admin/nfl-sync")
            .await
            .assert_status(StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn anonymous_is_redirected_from_nfl_sync() {
        let app = TestApp::new().await;
        let response = app.get("/admin/nfl-sync").await;
        assert!(response.status_code().is_redirection());
        assert!(
            response
                .header("location")
                .to_str()
                .unwrap()
                .starts_with("/login")
        );
    }
}
