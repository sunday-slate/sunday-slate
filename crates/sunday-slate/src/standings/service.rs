//! Assemble the standings: fetch each fantasy team's entries, resolve the
//! officially-decided contests, load their stats, score, rank.

use std::collections::HashMap;

use crate::contests::service;
use crate::entries::store as entries;
use crate::scoring::WeekStats;
use crate::standings::standings::{self, ScoredFantasyTeam, StandingRow};
use crate::{AppError, AppState};
use nfl_data::Week;
use nfl_data::slates::SlateId;

/// Every fantasy team in a league, scored against the supplied contest weeks
/// with their already-loaded stats. A contest absent from the map is skipped.
pub async fn scored_teams(
    state: &AppState,
    league_id: i64,
    contest_week: &HashMap<SlateId, Week>,
    stats: &HashMap<Week, WeekStats>,
) -> Result<Vec<ScoredFantasyTeam>, AppError> {
    let teams = entries::by_fantasy_team(state.db.reader(), league_id).await?;

    Ok(teams
        .into_iter()
        .map(|t| ScoredFantasyTeam {
            id: t.id,
            name: t.name,
            owner_name: t.owner_name,
            logo: t.logo.map(|m| m.url()),
            entered: t.entries.iter().map(|e| e.slate_id).collect(),
            scores: t
                .entries
                .iter()
                .filter_map(|entry| {
                    let week = contest_week.get(&entry.slate_id)?;
                    let week_stats = stats.get(week)?;
                    Some((
                        entry.slate_id,
                        crate::scoring::score_lineup(&entry.lineup, week_stats),
                    ))
                })
                .collect(),
        })
        .collect())
}

/// One league's ranked standings.
pub async fn standings_for_league(
    state: &AppState,
    league_id: i64,
) -> Result<Vec<StandingRow>, AppError> {
    let snapshot = state.nfl.snapshot();
    let results = service::official_contest_weeks(state, &snapshot).await?;
    Ok(standings::rank(
        scored_teams(state, league_id, &results.contest_week, &results.stats).await?,
    ))
}
