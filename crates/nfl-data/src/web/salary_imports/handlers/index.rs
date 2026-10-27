use std::collections::BTreeMap;

use crate::web::AdminState;
use crate::web::salary_imports::SalaryAdminError;
use crate::web::salary_imports::handlers::default_season_week;
use askama::Template;
use axum::extract::State;
use axum::response::Html;
use nfl_model::SeasonWeek;
use crate::WeeklySummary;

#[derive(Template)]
#[template(
    path = "admin/salary_imports/index.html",
    config = "src/web/askama.toml"
)]
pub(crate) struct DashboardTemplate {
    pub(crate) weeks: BTreeMap<SeasonWeek, SalaryCounts>,
    pub(crate) cur_week: SeasonWeek,
    pub(crate) config_season: u16,
}

pub(crate) struct SalaryCounts {
    pub(crate) salary_count: usize,
    pub(crate) unmatched_count: usize,
}

pub async fn index(State(state): State<AdminState>) -> Result<Html<String>, SalaryAdminError> {
    let cur_week = current_week(&state).await?;
    let weeks = salary_week_summaries(&state).await?;

    Ok(Html(
        DashboardTemplate {
            weeks,
            cur_week,
            config_season: state.config.season.0,
        }
        .render()?,
    ))
}

async fn salary_week_summaries(
    state: &AdminState,
) -> Result<BTreeMap<SeasonWeek, SalaryCounts>, SalaryAdminError> {
    let link_ids = state.nfl.store.links().await?;
    let matched_ids: Vec<u32> = link_ids.keys().copied().collect();
    let salary_counts = state.nfl.fanduel.weekly_salary_counts().await?;
    let unmatched_counts = state.nfl.fanduel.weekly_unmatched_counts(&matched_ids).await?;

    let summaries: Vec<WeeklySummary> = salary_counts
        .into_iter()
        .map(|(season_week, salary_count)| WeeklySummary {
            season_week,
            salary_count,
            unmatched_count: unmatched_counts.get(&season_week).copied().unwrap_or(0),
        })
        .collect();

    Ok(summaries
        .into_iter()
        .map(|summary| {
            (
                summary.season_week,
                SalaryCounts {
                    salary_count: summary.salary_count,
                    unmatched_count: summary.unmatched_count,
                },
            )
        })
        .collect())
}

async fn current_week(state: &AdminState) -> Result<SeasonWeek, SalaryAdminError> {
    let season = state.config.season;
    let games = state.nfl.games(season).await?;
    let now = state
        .config
        .now_override
        .unwrap_or_else(time::OffsetDateTime::now_utc);

    Ok(default_season_week(&games, season, now))
}
