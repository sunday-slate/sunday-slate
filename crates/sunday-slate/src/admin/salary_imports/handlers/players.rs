use crate::admin::salary_imports::{
    self as import,
    candidates::{Candidate, CandidateResults},
};
use crate::chrome::Chrome;
use crate::{AppError, AppState};
use askama::Template;
use axum::extract::{Path, Query, State};
use axum::response::{Html, IntoResponse};
use nfl_data::SeasonWeek;
use serde::Deserialize;

#[derive(Template)]
#[template(path = "admin/salary_imports/resolve.html")]
struct ResolutionTemplate {
    week: u32,
    fd_player_id: u32,
    source_name: String,
    team: String,
    position: String,
    salary: u32,
    suggestion: Option<Candidate>,
    results: CandidateResults,
    query: String,
    query_encoded: String,
    chrome: Chrome,
}

#[derive(Deserialize, Default)]
pub(super) struct PlayerSearch {
    #[serde(default)]
    pub(super) q: String,
    #[serde(default)]
    pub(super) scope: Option<String>,
}

pub async fn players(
    State(state): State<AppState>,
    Path((encoded, fd_player_id)): Path<(u32, u32)>,
    Query(query): Query<PlayerSearch>,
) -> Result<impl IntoResponse, AppError> {
    let season_week = SeasonWeek::decode(encoded).ok_or(AppError::NotFound)?;
    let row = unmatched_salary(&state, &season_week, fd_player_id).await?;
    let results = import::candidates::search(
        &state,
        &season_week,
        &row.team,
        &query.q,
        query.scope.as_deref() == Some("all"),
    )
    .await?;
    let player = row.player.as_ref().ok_or(AppError::NotFound)?;
    let suggestion = import::candidates::suggestion_for(&state, &season_week, &row).await?;
    Ok(Html(
        ResolutionTemplate {
            week: encoded,
            fd_player_id,
            source_name: format!("{} {}", player.first_name, player.last_name),
            team: row.team.as_str().to_owned(),
            position: String::from(row.position),
            salary: row.salary,
            suggestion,
            results,
            query_encoded: urlencoding::encode(&query.q).into_owned(),
            query: query.q,
            chrome: Chrome::focused(
                "Resolve salary player",
                format!("/admin/salary-imports/{encoded}"),
            ),
        }
        .render()?,
    ))
}

pub(super) async fn unmatched_salary(
    state: &AppState,
    season_week: &SeasonWeek,
    fd_player_id: u32,
) -> Result<nfl_data::SalariedPlayer, AppError> {
    let overview = import::salary_week_overview(&state.db, &state.nfl, *season_week)
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;
    overview
        .unmatched
        .into_iter()
        .find(|salary| {
            salary
                .player
                .as_ref()
                .is_some_and(|player| player.id == fd_player_id)
        })
        .ok_or(AppError::NotFound)
}

#[cfg(test)]
mod tests {
    use super::super::tests::{dashboard_app, upload_form};
    use http::StatusCode;

    #[tokio::test]
    async fn candidate_results_and_suggestions_exclude_already_mapped_gsis_ids() {
        let app = dashboard_app().await;
        app.server
            .post("/admin/salary-imports")
            .multipart(upload_form())
            .await
            .assert_status(StatusCode::SEE_OTHER);
        crate::nfl_players::store::upsert(&app.pool, "00-roster-only", 777)
            .await
            .unwrap();

        let resolution = app.get("/admin/salary-imports/2025001/players/999").await;
        assert!(!resolution.text().contains("Looks like Roster Receiver"));
        assert!(!resolution.text().contains("Confirm suggestion"));

        let candidates = app
            .get("/admin/salary-imports/2025001/players/999/search?q=Roster")
            .await;
        assert!(!candidates.text().contains("Roster Receiver (WR)"));
        candidates.assert_text_contains("No matching players found");
    }

    #[tokio::test]
    async fn resolution_page_shows_source_context_and_confirmable_weekly_suggestion() {
        let app = dashboard_app().await;
        app.server
            .post("/admin/salary-imports")
            .multipart(upload_form())
            .await
            .assert_status(StatusCode::SEE_OTHER);

        let response = app.get("/admin/salary-imports/2025001/players/999").await;
        response.assert_text_contains("Mystery Receiver");
        response.assert_text_contains("BUF · WR · $5000");
        response.assert_text_contains("Looks like");
        response.assert_text_contains("Roster Receiver (WR)");
        response.assert_text_contains("Confirm suggestion");
        response.assert_text_contains("name=\"gsis_player_id\" value=\"00-roster-only\"");
        assert!(
            crate::nfl_players::store::by_fd_id(&app.pool, 999)
                .await
                .unwrap()
                .is_none(),
            "showing a suggestion must not create the crosswalk"
        );
    }

    #[tokio::test]
    async fn changing_candidate_scope_preserves_the_search_query() {
        let app = dashboard_app().await;
        app.server
            .post("/admin/salary-imports")
            .multipart(upload_form())
            .await
            .assert_status(StatusCode::SEE_OTHER);

        let narrow = app
            .get("/admin/salary-imports/2025001/players/999?q=Josh")
            .await;
        narrow.assert_text_contains("q=Josh&amp;scope=all");
        let widened = app
            .get("/admin/salary-imports/2025001/players/999?q=Avery&scope=all")
            .await;
        widened.assert_text_contains("Avery Smith (WR)");
        widened.assert_text_contains("q=Avery");
        widened.assert_text_contains("Back to BUF players");
    }

    #[tokio::test]
    async fn players_rejects_unknown_source_player() {
        let app = dashboard_app().await;
        app.server
            .post("/admin/salary-imports")
            .multipart(upload_form())
            .await
            .assert_status(StatusCode::SEE_OTHER);
        app.get("/admin/salary-imports/2025001/players/404")
            .await
            .assert_status(StatusCode::NOT_FOUND);
    }
}
