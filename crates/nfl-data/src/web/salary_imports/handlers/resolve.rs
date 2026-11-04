use crate::web::salary_imports::SalaryAdminError;
use crate::web::{AdminState, salary_imports};
use crate::{NflDataError, PlayerLink, SalaryWeekOverview};
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Redirect};
use nfl_model::SeasonWeek;
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
        .identify(&[nfl_model::NflPlayerId(form.gsis_player_id.clone())])
        .await?;
    if !identities.contains_key(&nfl_model::NflPlayerId(form.gsis_player_id.clone())) {
        return Err(SalaryAdminError::BadRequest(
            "unknown GSIS player ID".into(),
        ));
    }
    resolve_salary_player(&state, season_week, fd_player_id, &form.gsis_player_id)
        .await
        .map_err(|e| SalaryAdminError::BadRequest(e.to_string()))?;
    Ok(Redirect::to(&format!(
        "/nfl-data-admin/salary-imports/{encoded}"
    )))
}

/// Confirm the link for one source row in the requested week; the link
/// applies globally. Other rows are left to their own resolution.
async fn resolve_salary_player(
    state: &AdminState,
    season_week: SeasonWeek,
    fd_player_id: u32,
    gsis_player_id: &str,
) -> Result<SalaryWeekOverview, NflDataError> {
    let source = state.nfl.fanduel.salaried_players_for(season_week).await?;
    if !source.iter().any(|salary| {
        salary
            .player
            .as_ref()
            .is_some_and(|player| player.id == fd_player_id)
    }) {
        return Err(NflDataError::Schedule);
    }
    state
        .nfl
        .save_player_links(&[PlayerLink {
            fd_player_id,
            gsis_player_id: gsis_player_id.to_owned(),
        }])
        .await?;
    state.nfl.rebuild_snapshot().await?;
    salary_imports::salary_week_overview(state, season_week).await
}

#[cfg(test)]
mod tests {
    use crate::DfsPosition;
    use crate::store::player_link;
    use crate::web::tests::salary_fixture::{
        admin_slate_id, admin_state, admin_state_in_season, upload_form,
    };
    use axum::http::StatusCode;
    use nfl_model::{DfsPositionSlotType, NflPlayerId};

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
        let links = player_link::links(state.nfl.store.reader()).await.unwrap();
        assert_eq!(links.get(&999).map(String::as_str), Some("00-roster-only"));
        let overview = crate::web::salary_imports::salary_week_overview(
            &crate::web::AdminState {
                nfl: std::sync::Arc::clone(&state.nfl),
                config: crate::web::AdminConfig {
                    season: crate::Season(2025),
                    now_override: None,
                },
            },
            crate::SeasonWeek {
                season: crate::Season(2025),
                season_type: crate::SeasonType::Reg,
                week: crate::Week(1),
            },
        )
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

    #[tokio::test]
    async fn resolving_publishes_the_linked_player_before_the_response() {
        let state = admin_state_in_season(crate::Season(2025)).await;

        // Upload first: Josh Allen auto-matches and publishes.
        state
            .server
            .post("/nfl-data-admin/salary-imports")
            .multipart(upload_form())
            .await
            .assert_status(StatusCode::SEE_OTHER);
        assert!(
            state
                .nfl
                .snapshot()
                .player(&admin_slate_id(), &NflPlayerId("00-1".into()))
                .is_some_and(|player| player.name == "Josh Allen")
        );

        // Manual resolution persists the link and republishes once.
        state
            .server
            .post("/nfl-data-admin/salary-imports/2025001/players/999/resolve")
            .form(&[("gsis_player_id", "00-roster-only")])
            .await
            .assert_status(StatusCode::SEE_OTHER);
        let snapshot = state.nfl.snapshot();
        assert!(
            snapshot
                .player(&admin_slate_id(), &NflPlayerId("00-roster-only".into()))
                .is_some_and(|player| player.name == "Roster Receiver"),
            "the completed resolution is visible immediately"
        );

        // Second Receiver (888) is still unmatched, so the WR bucket holds
        // only the resolved player.
        let wr = snapshot.salaried_players_for_position(
            &admin_slate_id(),
            &DfsPositionSlotType::Position(DfsPosition::Wr),
        );
        assert_eq!(
            wr.iter()
                .map(|player| player.name.as_str())
                .collect::<Vec<_>>(),
            ["Roster Receiver"],
            "the still-unmatched row published nothing"
        );
    }
}
