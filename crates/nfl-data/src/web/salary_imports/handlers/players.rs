use crate::web::AdminState;
use crate::web::salary_imports::SalaryAdminError;
use askama::Template;
use axum::extract::{Path, Query, State};
use axum::response::Html;

#[derive(Template)]
#[template(
    path = "admin/salary_imports/resolve.html",
    config = "src/web/askama.toml"
)]
struct ResolutionTemplate {
    week: u32,
    fd_player_id: u32,
    source_name: String,
    team: String,
    position: String,
    salary: u32,
    suggestion: Option<crate::salaries::candidates::Candidate>,
    results: crate::salaries::candidates::CandidateResults,
    query: String,
    query_encoded: String,
}

#[derive(serde::Deserialize, Default)]
pub(super) struct PlayerSearch {
    #[serde(default)]
    pub(super) q: String,
    #[serde(default)]
    pub(super) scope: Option<String>,
}

pub async fn players(
    State(state): State<AdminState>,
    Path((encoded, fd_player_id)): Path<(u32, u32)>,
    Query(query): Query<PlayerSearch>,
) -> Result<Html<String>, SalaryAdminError> {
    let season_week = crate::SeasonWeek::decode(encoded).ok_or(SalaryAdminError::NotFound)?;
    let row = unmatched_salary(&state, &season_week, fd_player_id).await?;
    let reconciled = state.nfl.reconciled_gsis_ids().await?;
    let results = crate::salaries::candidates::search(
        &state.nfl,
        &reconciled,
        &season_week,
        &row.team,
        &query.q,
        query.scope.as_deref() == Some("all"),
    )
    .await?;
    let player = row.player.as_ref().ok_or(SalaryAdminError::NotFound)?;
    let suggestion =
        crate::salaries::candidates::suggestion_for(&state.nfl, &reconciled, &season_week, &row)
            .await?;
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
        }
        .render()?,
    ))
}

pub(super) async fn unmatched_salary(
    state: &AdminState,
    season_week: &crate::SeasonWeek,
    fd_player_id: u32,
) -> Result<crate::SalariedPlayer, SalaryAdminError> {
    let overview = state.nfl.salary_week_overview(*season_week).await?;
    overview
        .unmatched
        .into_iter()
        .find(|salary| {
            salary
                .player
                .as_ref()
                .is_some_and(|player| player.id == fd_player_id)
        })
        .ok_or(SalaryAdminError::NotFound)
}

#[cfg(test)]
mod tests {
    use crate::web::tests::salary_fixture::{admin_state, upload_form};
    use axum::http::StatusCode;

    #[tokio::test]
    async fn candidate_results_and_suggestions_exclude_already_mapped_gsis_ids() {
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
            .form(&[("gsis_player_id", "00-roster-only")])
            .await
            .assert_status(StatusCode::SEE_OTHER);

        state
            .server
            .get("/nfl-data-admin/salary-imports/2025001/players/888?q=Roster")
            .await
            .assert_text_contains("No matching players found");
        state
            .server
            .get("/nfl-data-admin/salary-imports/2025001/players/888/search?q=Roster")
            .await
            .assert_text_contains("No matching players found");
    }

    #[tokio::test]
    async fn resolution_page_shows_source_context_and_confirmable_weekly_suggestion() {
        let state = admin_state().await;
        state
            .server
            .post("/nfl-data-admin/salary-imports")
            .multipart(upload_form())
            .await
            .assert_status(StatusCode::SEE_OTHER);

        let response = state
            .server
            .get("/nfl-data-admin/salary-imports/2025001/players/999")
            .await;
        response.assert_text_contains("Mystery Receiver");
        response.assert_text_contains("BUF · WR · $5000");
        response.assert_text_contains("Looks like");
        response.assert_text_contains("Roster Receiver (WR)");
        response.assert_text_contains("Confirm suggestion");
        response.assert_text_contains("name=\"gsis_player_id\" value=\"00-roster-only\"");
    }

    #[tokio::test]
    async fn changing_candidate_scope_preserves_the_search_query() {
        let state = admin_state().await;
        state
            .server
            .post("/nfl-data-admin/salary-imports")
            .multipart(upload_form())
            .await
            .assert_status(StatusCode::SEE_OTHER);

        let narrow = state
            .server
            .get("/nfl-data-admin/salary-imports/2025001/players/999?q=Josh")
            .await;
        narrow.assert_text_contains("q=Josh&amp;scope=all");
        let widened = state
            .server
            .get("/nfl-data-admin/salary-imports/2025001/players/999?q=Avery&scope=all")
            .await;
        widened.assert_text_contains("Avery Smith (WR)");
        widened.assert_text_contains("q=Avery");
        widened.assert_text_contains("Back to BUF players");
    }

    #[tokio::test]
    async fn players_rejects_unknown_source_player() {
        let state = admin_state().await;
        state
            .server
            .post("/nfl-data-admin/salary-imports")
            .multipart(upload_form())
            .await
            .assert_status(StatusCode::SEE_OTHER);
        state
            .server
            .get("/nfl-data-admin/salary-imports/2025001/players/404")
            .await
            .assert_status(StatusCode::NOT_FOUND);
    }
}
