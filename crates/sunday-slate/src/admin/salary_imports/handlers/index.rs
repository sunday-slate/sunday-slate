use crate::admin::salary_imports::default_season_week;
use crate::chrome::Chrome;
use crate::nfl_players::store as nfl_players_store;
use crate::{AppError, AppState};
use askama::Template;
use axum::extract::State;
use axum::response::{Html, IntoResponse};
use nfl_model::{Season, SeasonWeek};
use std::collections::BTreeMap;

#[derive(Template)]
#[template(path = "admin/salary_imports/index.html")]
pub(crate) struct DashboardTemplate {
    pub(crate) weeks: BTreeMap<SeasonWeek, SalaryCounts>,
    pub(crate) cur_week: SeasonWeek,
    pub(crate) chrome: Chrome,
}

pub(crate) struct SalaryCounts {
    pub(crate) salary_count: usize,
    pub(crate) unmatched_count: usize,
}
pub async fn index(State(state): State<AppState>) -> Result<impl IntoResponse, AppError> {
    let cur_week = current_week(&state).await?;
    let weeks = fd_week_summaries(&state).await?;

    Ok(Html(
        DashboardTemplate {
            weeks,
            cur_week,
            chrome: Chrome::focused("Salary weeks", "/admin"),
        }
        .render()?,
    ))
}

async fn fd_week_summaries(
    state: &AppState,
) -> Result<BTreeMap<SeasonWeek, SalaryCounts>, AppError> {
    let matched_ids: Vec<u32> = nfl_players_store::mapped_fd_ids(state.db.reader())
        .await?
        .into_iter()
        .collect();

    let summaries = state
        .nfl
        .fanduel_weekly_summaries(&matched_ids)
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?
        .iter()
        .map(|summary| {
            (
                summary.season_week,
                SalaryCounts {
                    salary_count: summary.salary_count,
                    unmatched_count: summary.unmatched_count,
                },
            )
        })
        .collect();

    Ok(summaries)
}

async fn current_week(state: &AppState) -> Result<SeasonWeek, AppError> {
    let season = Season(state.config.season);
    let games = state
        .nfl
        .games(season)
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;

    Ok(default_season_week(&games, season, state.config.now()))
}

#[cfg(test)]
mod tests {
    use super::super::tests::{DASHBOARD_CSV, dashboard_app, upload_form};
    use crate::tests::factories::UserOptions;
    use crate::tests::{TestApp, factories};
    use http::StatusCode;
    use nfl_data::{Game, Season, SeasonType, Week};

    #[tokio::test]
    async fn non_admin_cannot_open_salaries() {
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
        app.get("/admin/salary-imports")
            .await
            .assert_status(StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn admin_sees_dashboard() {
        let app = TestApp::new().await;
        app.login_admin().await;
        app.get("/admin/salary-imports").await.assert_status_ok();
    }

    #[tokio::test]
    async fn dashboard_lists_source_counts_and_resolves_roster_only_players_directly() {
        let app = dashboard_app().await;
        app.server
            .post("/admin/salary-imports")
            .multipart(upload_form())
            .await
            .assert_status(StatusCode::SEE_OTHER);
        let salaries = crate::player_salaries::store::for_games(&app.pool, &["2025_01_BUF_NYJ"])
            .await
            .unwrap();
        assert_eq!(
            salaries.len(),
            2,
            "mapped QB and DST are immediately usable"
        );
        assert!(
            salaries
                .iter()
                .any(|s| s.dfs_position == nfl_data::DfsPosition::Dst)
        );
        let root = app.get("/admin/salary-imports").await;
        root.assert_status_ok();
        root.assert_text_contains("3 salaries");
        root.assert_text_contains("1 unresolved");
        root.assert_text_contains("/admin/salary-imports/2025001");
        let detail = app.get("/admin/salary-imports/2025001").await;
        detail.assert_status_ok();
        detail.assert_text_contains("Mystery Receiver");
        detail.assert_text_contains("/admin/salary-imports/2025001/players/999");
        assert!(!detail.text().contains("Josh Allen"));
        let resolution = app.get("/admin/salary-imports/2025001/players/999").await;
        resolution.assert_status_ok();
        resolution.assert_text_contains("Confirm suggestion");
        let candidates = app
            .get("/admin/salary-imports/2025001/players/999/search?q=Roster")
            .await;
        candidates.assert_status_ok();
        candidates.assert_text_contains("Roster Receiver (WR)");
        candidates.assert_text_contains("on BUF");
        assert!(!candidates.text().contains("week 1"));
        assert!(!candidates.text().contains(">00-roster-only<"));
        app.server
            .post("/admin/salary-imports/2025001/players/999/resolve")
            .form(&[("gsis_player_id", "00-roster-only")])
            .await
            .assert_status(StatusCode::SEE_OTHER);
        let resolved = app.get("/admin/salary-imports/2025001").await;
        resolved.assert_text_contains("All player IDs are resolved");
        assert_eq!(
            crate::player_salaries::store::for_games(&app.pool, &["2025_01_BUF_NYJ"])
                .await
                .unwrap()
                .len(),
            3
        );
        assert_eq!(
            crate::nfl_players::store::by_fd_id(&app.pool, 999)
                .await
                .unwrap()
                .unwrap()
                .gsis_player_id,
            "00-roster-only"
        );
    }

    #[tokio::test]
    async fn dashboard_groups_sources_by_season_type_and_week() {
        let app = dashboard_app().await;
        for season_week in [
            nfl_data::SeasonWeek {
                season: Season(2025),
                season_type: SeasonType::Reg,
                week: Week(1),
            },
            nfl_data::SeasonWeek {
                season: Season(2025),
                season_type: SeasonType::Post,
                week: Week(21),
            },
            nfl_data::SeasonWeek {
                season: Season(2026),
                season_type: SeasonType::Reg,
                week: Week(1),
            },
        ] {
            let salaries = app
                .nfl
                .parse_fanduel_csv(season_week, DASHBOARD_CSV.as_bytes())
                .unwrap();
            app.nfl.save_fanduel_salaries(&salaries).await.unwrap();
        }
        let root = app.get("/admin/salary-imports").await;
        root.assert_status_ok();
        for encoded in [2025001, 2025121, 2026001] {
            root.assert_text_contains(format!("/admin/salary-imports/{encoded}"));
        }
        assert_eq!(root.text().matches("3 salaries").count(), 3);
        assert_eq!(root.text().matches("2 unresolved").count(), 3);
    }

    #[tokio::test]
    async fn dashboard_form_uses_config_clock_at_the_monday_boundary() {
        use time::macros::datetime;
        let app = TestApp::from_pool_at(
            crate::tests::utils::in_memory_pool().await,
            datetime!(2025-10-06 18:00 UTC),
        )
        .await;
        app.login_admin().await;
        let game = |id: &str, week, kickoff| Game {
            gsis_game_id: id.into(),
            season: Season(2025),
            week: Week(week),
            season_type: SeasonType::Reg,
            kickoff: Some(kickoff),
            home_team: "NYJ".parse().unwrap(),
            away_team: "BUF".parse().unwrap(),
            home_score: None,
            away_score: None,
        };
        app.nfl
            .seed_for_test(
                &[],
                &[
                    game("monday", 5, datetime!(2025-10-06 23:15 UTC)),
                    game("upcoming", 6, datetime!(2025-10-12 17:00 UTC)),
                ],
            )
            .await
            .unwrap();
        app.get("/admin/salary-imports")
            .await
            .assert_text_contains("value=\"6\"");
    }
}
