use crate::contests::service as contests_service;
use crate::entries::store as entries;
use crate::scoring::service as scoring_service;
use crate::standings::standings::{self, ScoredFantasyTeam, StandingRow};
use crate::{AppError, AppState};

pub async fn scored_teams(
    state: &AppState,
    league_id: i64,
) -> Result<Vec<ScoredFantasyTeam>, AppError> {
    let results = contests_service::official_contest_weeks(state).await?;
    let teams = entries::by_fantasy_team(state.db.reader(), league_id).await?;

    Ok(teams
        .into_iter()
        .map(|t| ScoredFantasyTeam {
            id: t.id,
            name: t.name,
            owner_name: t.owner_name,
            logo: t.logo.map(|m| m.url()),
            entered: t.entries.iter().map(|e| e.slate_id).collect(),
            scores: scoring_service::score_entries(&t.entries, &results.snapshot, &results.slates),
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
