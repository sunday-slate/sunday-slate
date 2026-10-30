use crate::{NflData, NflDataError};
use nfl_model::{Game, SeasonWeek};

pub(crate) async fn games(nfl: &NflData, week: &SeasonWeek) -> Result<Vec<Game>, NflDataError> {
    let games = nfl
        .provider
        .games(week.season)
        .await?
        .iter()
        .filter(|g| g.season_week == *week)
        .cloned()
        .collect();
    Ok(games)
}
