use super::players::{PlayerSearch, unmatched_salary};
use crate::admin::salary_imports::{self as import, candidates::CandidateResults};
use crate::{AppError, AppState};
use askama::Template;
use axum::extract::{Path, Query, State};
use axum::response::{Html, IntoResponse};
use nfl_data::SeasonWeek;

#[derive(Template)]
#[template(path = "admin/salary_imports/_results.html")]
struct CandidateResultsTemplate {
    week: u32,
    fd_player_id: u32,
    team: String,
    results: CandidateResults,
    query_encoded: String,
}
pub async fn player_search(
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
    use super::super::tests::{dashboard_app, upload_form};
    use http::StatusCode;

    #[tokio::test]
    async fn candidate_search_keeps_team_scope_until_explicitly_widened() {
        let app = dashboard_app().await;
        app.server
            .post("/admin/salary-imports")
            .multipart(upload_form())
            .await
            .assert_status(StatusCode::SEE_OTHER);
        app.get("/admin/salary-imports/2025001/players/999?q=Roster")
            .await
            .assert_text_contains("Roster Receiver");
        let narrow = app
            .get("/admin/salary-imports/2025001/players/999/search?q=Josh")
            .await;
        narrow.assert_text_contains("No matching players found");
        narrow.assert_text_contains("on BUF");
        assert!(!narrow.text().contains("week 1"));
        let wide = app
            .get("/admin/salary-imports/2025001/players/999/search?q=Avery&scope=all")
            .await;
        wide.assert_text_contains("Avery Smith (WR)");
        wide.assert_text_contains("across all teams");
        assert!(!wide.text().contains("weeks"));
        let one_character = app
            .get("/admin/salary-imports/2025001/players/999/search?scope=all&q=A")
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
        let app = dashboard_app().await;
        app.server
            .post("/admin/salary-imports")
            .multipart(upload_form())
            .await
            .assert_status(StatusCode::SEE_OTHER);

        let page = app.get("/admin/salary-imports/2025001/players/999").await;
        page.assert_text_contains("hx-trigger=\"focus once, input changed delay:300ms\"");
        page.assert_text_contains("hx-target=\"#candidate-results\"");
        assert_eq!(
            page.text().matches("name=\"gsis_player_id\"").count(),
            2,
            "the initial view shows the suggestion and the current candidate list"
        );
        assert!(!page.text().contains("ID 999"));
        assert!(
            !page
                .text()
                .contains("Roster Receiver (WR) · 00-roster-only")
        );
        page.assert_text_contains("Roster Receiver (WR)");

        let empty = app
            .get("/admin/salary-imports/2025001/players/999/search")
            .await;
        empty.assert_text_contains("Roster Receiver (WR)");
        assert!(!empty.text().contains("Type a name to filter"));

        let results = app
            .get("/admin/salary-imports/2025001/players/999/search?q=Roster")
            .await;
        results.assert_text_contains("Roster Receiver (WR)");
        results.assert_text_contains("on BUF");
        assert!(!results.text().contains("week 1"));
        assert!(!results.text().contains("<html"));

        app.get("/admin/salary-imports/2025001/players/999/search?q=Avery&scope=all")
            .await
            .assert_text_contains("Avery Smith (WR)");
        app.get("/admin/salary-imports/2025001/players/999/search?scope=all")
            .await
            .assert_text_contains("Avery Smith (WR)");
    }
}
