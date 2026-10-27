use crate::web::AdminState;
use crate::web::salary_imports::SalaryAdminError;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Redirect};
use serde::Deserialize;

#[derive(Deserialize)]
pub struct ResolveForm {
    gsis_player_id: String,
}

pub async fn resolve(
    State(state): State<AdminState>,
    Path((encoded, fd_player_id)): Path<(u32, u32)>,
    axum::Form(form): axum::Form<ResolveForm>,
) -> Result<impl IntoResponse, SalaryAdminError> {
    let season_week = crate::SeasonWeek::decode(encoded).ok_or(SalaryAdminError::NotFound)?;
    let identities = state
        .nfl
        .identify(&[form.gsis_player_id.as_str()])
        .await
        .map_err(|e| SalaryAdminError::Internal(e.to_string()))?;
    if !identities.contains_key(&form.gsis_player_id) {
        return Err(SalaryAdminError::BadRequest(
            "unknown GSIS player ID".into(),
        ));
    }
    state
        .nfl
        .resolve_salary_player(season_week, fd_player_id, &form.gsis_player_id)
        .await
        .map_err(|e| SalaryAdminError::BadRequest(e.to_string()))?;
    Ok(Redirect::to(&format!(
        "/nfl-data-admin/salary-imports/{encoded}"
    )))
}

#[cfg(test)]
mod tests {
    use crate::web::tests::salary_fixture::{admin_state, upload_form};
    use axum::http::StatusCode;

    #[tokio::test]
    async fn resolve_rejects_unknown_players() {
        let state = admin_state().await;
        state
            .server
            .post("/nfl-data-admin/salary-imports")
            .multipart(upload_form())
            .await
            .assert_status(StatusCode::SEE_OTHER);
        state
            .server
            .post("/nfl-data-admin/salary-imports/2025001/players/999/resolve")
            .form(&[("gsis_player_id", "not-a-player")])
            .await
            .assert_status(StatusCode::BAD_REQUEST);
        state
            .server
            .post("/nfl-data-admin/salary-imports/2025001/players/404/resolve")
            .form(&[("gsis_player_id", "00-1")])
            .await
            .assert_status(StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn resolve_creates_crosswalk_and_confirms_link() {
        let state = admin_state().await;
        state
            .server
            .post("/nfl-data-admin/salary-imports")
            .multipart(upload_form())
            .await
            .assert_status(StatusCode::SEE_OTHER);

        let response = state
            .server
            .post("/nfl-data-admin/salary-imports/2025001/players/999/resolve")
            .form(&[("gsis_player_id", "00-roster-only")])
            .await;
        response.assert_status(StatusCode::SEE_OTHER);
        assert_eq!(
            response.header("location"),
            "/nfl-data-admin/salary-imports/2025001"
        );
        assert_eq!(
            state
                .nfl
                .player_link_by_fd_id(999)
                .await
                .unwrap()
                .expect("link")
                .gsis_player_id,
            "00-roster-only"
        );
        let overview = state
            .nfl
            .salary_week_overview(crate::SeasonWeek {
                season: crate::Season(2025),
                season_type: crate::SeasonType::Reg,
                week: crate::Week(1),
            })
            .await
            .unwrap();
        assert_eq!(overview.unmatched.len(), 1);
        assert_eq!(overview.unmatched[0].player.as_ref().unwrap().id, 888);
        let page = state
            .server
            .get("/nfl-data-admin/salary-imports/2025001")
            .await;
        page.assert_text_contains("Second Receiver");
        assert!(!page.text().contains("All player IDs are resolved"));
    }
}
