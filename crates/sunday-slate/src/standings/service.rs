//! Assemble the standings: fetch each fantasy team's entries, resolve
//! contests to NFL weeks, load those weeks' stats, score, rank.

use std::collections::HashMap;

use crate::contests::ContestId;
use crate::contests::service::SeasonSlates;
use crate::entries::store as entries;
use crate::scoring::service::week_stats;
use crate::standings::standings::{self, ScoredFantasyTeam, StandingRow};
use crate::{AppError, AppState};
use nfl_data::{Season, Week};

/// Every fantasy team in a league, scored against the supplied contest weeks.
/// A contest absent from the map is skipped, so a fantasy team with only
/// unscored entries stays visible with no scores. The contest list and the
/// standings build on this.
pub async fn scored_teams(
    state: &AppState,
    league_id: i64,
    contest_week: &HashMap<ContestId, Week>,
) -> Result<Vec<ScoredFantasyTeam>, AppError> {
    let stats = week_stats(
        &state.nfl,
        Season(state.config.season),
        contest_week.values().copied(),
    )
    .await?;
    let teams = entries::by_fantasy_team(state.db.reader(), league_id).await?;

    Ok(teams
        .into_iter()
        .map(|t| ScoredFantasyTeam {
            id: t.id,
            name: t.name,
            owner_name: t.owner_name,
            logo: t.logo.map(|m| m.url()),
            entered: t.entries.iter().map(|e| e.contest).collect(),
            scores: t
                .entries
                .iter()
                .filter_map(|entry| {
                    let week = contest_week.get(&entry.contest)?;
                    let week_stats = stats.get(week)?;
                    Some((
                        entry.contest,
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
    let season = Season(state.config.season);
    let slates = SeasonSlates::load(state.db.reader(), &state.nfl, season).await?;
    let contest_week = slates.finished_contest_weeks(state.now_eastern());
    Ok(standings::rank(
        scored_teams(state, league_id, &contest_week).await?,
    ))
}
