//! Assemble the standings: fetch each fantasy team's entries, resolve the
//! officially-decided contests to NFL weeks, load those weeks' stats, score,
//! rank.

use std::collections::{HashMap, HashSet};

use crate::contests::ContestId;
use crate::contests::service::SeasonSlates;
use crate::entries::store as entries;
use crate::scoring::WeekStats;
use crate::standings::standings::{self, ScoredFantasyTeam, StandingRow};
use crate::{AppError, AppState};
use nfl_data::{Season, Week};

/// Every fantasy team in a league, scored against the supplied contest weeks
/// using their already-loaded stats. A contest absent from the map is
/// skipped, so a fantasy team with only unscored entries stays visible with
/// no scores. The contest list and the standings build on this.
pub async fn scored_teams(
    state: &AppState,
    league_id: i64,
    contest_week: &HashMap<ContestId, Week>,
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

/// One league's ranked standings over its officially-decided contests.
pub async fn standings_for_league(
    state: &AppState,
    league_id: i64,
) -> Result<Vec<StandingRow>, AppError> {
    let season = Season(state.config.season);
    let slates = SeasonSlates::load(state.db.reader(), &state.nfl, season).await?;
    let wanted: HashSet<ContestId> = slates.slates.keys().copied().collect();
    let results = slates.official_contest_weeks(state, &wanted).await?;
    Ok(standings::rank(
        scored_teams(state, league_id, &results.contest_week, &results.stats).await?,
    ))
}
