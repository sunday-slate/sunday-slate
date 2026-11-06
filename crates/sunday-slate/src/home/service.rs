//! Loads the season's contests and resolves the Current Contest.

use crate::contests::ContestId;
use crate::contests::service;
use crate::contests::store;
use crate::home::dispatch;
use crate::{AppError, AppState};

/// The Current Contest, resolved from the published snapshot's slates.
/// `None` when no contest resolves a slate.
pub async fn current_contest_id(state: &AppState) -> Result<Option<ContestId>, AppError> {
    let snapshot = state.nfl.snapshot();
    let contests = service::slate_contests(&store::all(state.db.reader()).await?, &snapshot);
    Ok(dispatch::current_contest(&contests, state.now_eastern()))
}
