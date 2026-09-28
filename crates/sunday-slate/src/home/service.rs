//! Loads the season's contests and resolves the Current Contest.

use nfl_data::Season;

use crate::contests::ContestId;
use crate::contests::service::SeasonSlates;
use crate::contests::store;
use crate::home::dispatch;
use crate::{AppError, AppState};

/// The Current Contest, resolved from the configured season's slates.
/// `None` when the season has no contests.
pub async fn current_contest_id(state: &AppState) -> Result<Option<ContestId>, AppError> {
    let season = Season(state.config.season);
    let slates = SeasonSlates::load(state.db.reader(), &state.nfl, season).await?;
    let contests = slates.slate_contests(&store::all(state.db.reader()).await?);
    Ok(dispatch::current_contest(&contests, state.now_eastern()))
}
