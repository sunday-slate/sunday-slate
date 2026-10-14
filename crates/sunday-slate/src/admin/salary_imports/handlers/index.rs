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
