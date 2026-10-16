use crate::admin::salary_imports as import;
use crate::{AppError, AppState};
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Redirect};
use nfl_data::SeasonWeek;
use serde::Deserialize;

#[derive(Deserialize)]
pub struct ResolveForm {
    gsis_player_id: String,
}

pub async fn resolve(
    State(state): State<AppState>,
    Path((encoded, fd_player_id)): Path<(u32, u32)>,
    axum::Form(form): axum::Form<ResolveForm>,
) -> Result<impl IntoResponse, AppError> {
    let season_week = SeasonWeek::decode(encoded).ok_or(AppError::NotFound)?;
    let identities = state
        .nfl
        .identify(&[form.gsis_player_id.as_str()])
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;
    if !identities.contains_key(&form.gsis_player_id) {
        return Err(AppError::BadRequest("unknown GSIS player ID".into()));
    }
    import::resolve_salary_player(
        &state.db,
        &state.nfl,
        season_week,
        fd_player_id,
        &form.gsis_player_id,
    )
    .await
    .map_err(|e| AppError::BadRequest(e.to_string()))?;
    Ok(Redirect::to(&format!("/admin/salary-imports/{encoded}")))
}

#[cfg(test)]
mod tests {
    use super::super::tests::{dashboard_app, upload_form};
    use http::StatusCode;

    #[tokio::test]
    async fn resolve_rejects_unknown_players() {
        let app = dashboard_app().await;
        app.server
            .post("/admin/salary-imports")
            .multipart(upload_form())
            .await
            .assert_status(StatusCode::SEE_OTHER);
        app.server
            .post("/admin/salary-imports/2025001/players/999/resolve")
            .form(&[("gsis_player_id", "not-a-player")])
            .await
            .assert_status(StatusCode::BAD_REQUEST);
        app.server
            .post("/admin/salary-imports/2025001/players/404/resolve")
            .form(&[("gsis_player_id", "00-1")])
            .await
            .assert_status(StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn resolve_creates_crosswalk_and_projects_salary() {
        let app = dashboard_app().await;
        app.server
            .post("/admin/salary-imports")
            .multipart(upload_form())
            .await
            .assert_status(StatusCode::SEE_OTHER);

        let response = app
            .server
            .post("/admin/salary-imports/2025001/players/999/resolve")
            .form(&[("gsis_player_id", "00-roster-only")])
            .await;
        response.assert_status(StatusCode::SEE_OTHER);
        assert_eq!(response.header("location"), "/admin/salary-imports/2025001");
        assert_eq!(
            crate::nfl_players::store::by_fd_id(&app.pool, 999)
                .await
                .unwrap()
                .unwrap()
                .gsis_player_id,
            "00-roster-only"
        );
        assert_eq!(
            crate::player_salaries::store::for_games(&app.pool, &["2025_01_BUF_NYJ"])
                .await
                .unwrap()
                .len(),
            3
        );
        app.get("/admin/salary-imports/2025001")
            .await
            .assert_text_contains("All player IDs are resolved");
    }
}
