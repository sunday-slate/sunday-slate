use super::players::{PlayerSearch, unmatched_salary};
use crate::salaries::candidates::CandidateResults;
use crate::web::AdminState;
use crate::web::salary_imports::SalaryAdminError;
use askama::Template;
use axum::extract::{Path, Query, State};
use axum::response::Html;

#[derive(Template)]
#[template(
    path = "admin/salary_imports/_results.html",
    config = "src/web/askama.toml"
)]
struct CandidateResultsTemplate {
    week: u32,
    fd_player_id: u32,
    team: String,
    results: CandidateResults,
    query_encoded: String,
}
pub async fn player_search(
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
    Ok(Html(
        CandidateResultsTemplate {
            week: encoded,
            fd_player_id,
            team: row.team.as_str().to_owned(),
            results,
            query_encoded: urlencoding::encode(&query.q).into_owned(),
        }
        .render()?,
    ))
}

#[cfg(test)]
mod tests {
    use crate::web::tests::salary_fixture::{admin_state, upload_form};
    use axum::http::StatusCode;

    #[tokio::test]
    async fn candidate_search_keeps_team_scope_until_explicitly_widened() {
        let state = admin_state().await;
        state
            .server
            .post("/salary-imports")
            .multipart(upload_form())
            .await
            .assert_status(StatusCode::SEE_OTHER);
        state
            .server
            .get("/salary-imports/2025001/players/999?q=Roster")
            .await
            .assert_text_contains("Roster Receiver");
        let narrow = state
            .server
            .get("/salary-imports/2025001/players/999/search?q=Josh")
            .await;
        narrow.assert_text_contains("No matching players found");
        narrow.assert_text_contains("on BUF");
        assert!(!narrow.text().contains("week 1"));
        let wide = state
            .server
            .get("/salary-imports/2025001/players/999/search?q=Avery&scope=all")
            .await;
        wide.assert_text_contains("Avery Smith (WR)");
        wide.assert_text_contains("across all teams");
        assert!(!wide.text().contains("weeks"));
        let one_character = state
            .server
            .get("/salary-imports/2025001/players/999/search?scope=all&q=A")
            .await;
        one_character.assert_text_contains("Avery Smith (WR)");
        assert!(
            !one_character
                .text()
                .contains("Type at least two characters")
        );
    }

    #[tokio::test]
    async fn resolution_typeahead_returns_a_fragment_without_displaying_ids() {
        let state = admin_state().await;
        state
            .server
            .post("/salary-imports")
            .multipart(upload_form())
            .await
            .assert_status(StatusCode::SEE_OTHER);
        let fragment = state
            .server
            .get("/salary-imports/2025001/players/999/search?q=Roster")
            .await;
        fragment.assert_status_ok();
        assert!(!fragment.text().contains("<!doctype html>"));
        assert!(fragment.text().contains("Roster Receiver (WR)"));
    }
}
