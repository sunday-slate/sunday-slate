use crate::admin::salary_imports::default_season_week;
use crate::chrome::Chrome;
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
    let weeks = week_summaries(&state).await?;

    Ok(Html(
        DashboardTemplate {
            weeks,
            cur_week,
            chrome: Chrome::focused("Salary weeks", "/admin"),
        }
        .render()?,
    ))
}

async fn week_summaries(state: &AppState) -> Result<BTreeMap<SeasonWeek, SalaryCounts>, AppError> {
    let salary_counts = state
        .nfl
        .salary_counts_by_week()
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;

    let weeks: Vec<&SeasonWeek> = salary_counts.keys().collect();
    let unmatched_counts = unmatched_counts(state, &weeks).await?;

    let summaries = salary_counts
        .into_iter()
        .map(|(week, salary_count)| {
            let unmatched_count = unmatched_counts.get(&week).copied().unwrap_or(0);
            (
                week,
                SalaryCounts {
                    salary_count: salary_count as usize,
                    unmatched_count,
                },
            )
        })
        .collect();

    Ok(summaries)
}

async fn unmatched_counts(
    state: &AppState,
    weeks: &[&SeasonWeek],
) -> Result<BTreeMap<SeasonWeek, usize>, AppError> {
    let mut res = vec![];
    for season_week in weeks {
        let overview = crate::admin::salary_imports::salary_week_overview(
            &state.db,
            &state.nfl,
            **season_week,
        )
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;
        res.push((**season_week, overview.unmatched.len()))
    }

    Ok(res.into_iter().collect())
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
