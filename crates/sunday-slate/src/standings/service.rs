//! Assemble the standings: fetch each fantasy team's entries, score the ones
//! in officially-decided contests from the snapshot, rank.

use crate::contests::service;
use crate::entries::store as entries;
use crate::standings::standings::{self, ScoredFantasyTeam, StandingRow};
use crate::{AppError, AppState};

/// Every fantasy team in a league, scored against the published snapshot's
/// official slates.
pub async fn scored_teams(
    state: &AppState,
    league_id: i64,
) -> Result<Vec<ScoredFantasyTeam>, AppError> {
    let results = service::official_contest_weeks(state).await?;
    let teams = entries::by_fantasy_team(state.db.reader(), league_id).await?;

    Ok(teams
        .into_iter()
        .map(|t| ScoredFantasyTeam {
            id: t.id,
            name: t.name,
            owner_name: t.owner_name,
            logo: t.logo.map(|m| m.url()),
            entered: t.entries.iter().map(|e| e.slate_id).collect(),
            scores: crate::scoring::service::score_entries(
                &t.entries,
                &results.snapshot,
                &results.slates,
            ),
        })
        .collect())
}

/// One league's ranked standings.
pub async fn standings_for_league(
    state: &AppState,
    league_id: i64,
) -> Result<Vec<StandingRow>, AppError> {
    Ok(standings::rank(scored_teams(state, league_id).await?))
}
