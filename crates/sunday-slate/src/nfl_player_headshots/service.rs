//! Headshot discovery.

use std::collections::HashSet;

use nfl_model::NflPlayerId;

use crate::nfl_player_headshots::store;
use crate::{AppError, AppState};

/// Uncached players on the latest slate. Cache = table row; unidentifiable or
/// sourceless players retry next run.
pub(crate) async fn missing_headshots(state: &AppState) -> Result<Vec<NflPlayerId>, AppError> {
    let snapshot = state.nfl.snapshot();
    let Some(slate) = snapshot.current_slate() else {
        return Ok(Vec::new());
    };
    let Some(players) = snapshot.players_for_slate(&slate.id) else {
        return Ok(Vec::new());
    };
    if players.is_empty() {
        return Ok(Vec::new());
    }

    let cached: HashSet<NflPlayerId> = store::cached_player_ids(state.db.reader())
        .await?
        .into_iter()
        .collect();

    // order unspecified; treat as a set
    Ok(players
        .keys()
        .filter(|id| !cached.contains(*id))
        .cloned()
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::TestApp;

    #[tokio::test]
    async fn a_snapshot_without_slates_has_no_candidates() {
        let app = TestApp::new().await;
        assert!(
            missing_headshots(&app.state())
                .await
                .expect("discovery")
                .is_empty(),
            "nothing to fetch before a slate is published"
        );
    }
}
