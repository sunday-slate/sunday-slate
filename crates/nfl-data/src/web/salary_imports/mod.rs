pub(crate) mod error;
pub(crate) mod handlers;

pub(crate) use error::SalaryAdminError;
pub use handlers::router;
use nfl_model::SeasonWeek;
use crate::{SalaryError, SalaryWeekOverview};
use crate::web::AdminState;

/// Every source row for the week plus the player IDs lacking a link.
async fn salary_week_overview(
    state: &AdminState,
    season_week: SeasonWeek,
) -> Result<SalaryWeekOverview, SalaryError> {
    let salaries = state.nfl.fanduel.salaried_players_for(season_week).await?;
    let salary_count = salaries.len();

    let links = state.nfl.store.links().await?;
    let unmatched = salaries
        .into_iter()
        .filter(|salaried_player| match &salaried_player.player {
            Some(player) => !links.contains_key(&player.id),
            None => false,
        })
        .collect();

    Ok(SalaryWeekOverview {
        season_week,
        salary_count,
        unmatched,
    })
}