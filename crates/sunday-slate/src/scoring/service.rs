//! Scoring assembly a handler can call: load the stats a set of contests
//! needs, then score each fantasy team's entries against them.

use std::collections::{HashMap, HashSet};

use nfl_data::{NflData, NflDataError, Season, Week};

use crate::contests::ContestId;
use crate::entries::{FantasyTeamEntry, NflPlayerId};
use crate::scoring::{self, WeekStats};

/// Season-to-date FPPG and games played for each gsis id in `ids`, from the
/// weeks before `before`. Empty before week 2.
pub async fn season_averages(
    nfl: &NflData,
    season: Season,
    ids: &[impl AsRef<str>],
    before: Week,
) -> Result<HashMap<String, (f64, u32)>, NflDataError> {
    let wanted: HashSet<&str> = ids.iter().map(|i| i.as_ref()).collect();
    Ok(nfl
        .player_season_totals(season, before)
        .await?
        .into_iter()
        .filter(|t| wanted.contains(t.gsis_id.as_str()))
        .map(|t| (t.gsis_id, (t.fantasy_points_ppr / t.games as f64, t.games)))
        .collect())
}

/// Load each distinct week's stats at most once, keyed for lineup scoring.
/// Duplicate weeks are queried once even when the week is omitted from the
/// result.
///
/// A week is included only when both its player and defense feeds have rows.
/// If either feed is missing — not played yet, or played but not yet synced —
/// the week gets no entry, so `score_entries` skips it instead of scoring every
/// lineup a false zero.
pub async fn week_stats(
    nfl: &NflData,
    season: Season,
    weeks: impl IntoIterator<Item = Week>,
) -> Result<HashMap<Week, WeekStats>, NflDataError> {
    let mut stats: HashMap<Week, WeekStats> = HashMap::new();
    let mut seen: HashSet<Week> = HashSet::new();
    for week in weeks {
        if !seen.insert(week) {
            continue;
        }
        let players: HashMap<NflPlayerId, _> = nfl
            .player_week_stats(season, week)
            .await?
            .into_iter()
            .map(|p| (NflPlayerId(p.gsis_id.clone()), p))
            .collect();
        let defenses: HashMap<_, _> = nfl
            .team_week_stats(season, week)
            .await?
            .into_iter()
            .map(|t| (t.team.clone(), t))
            .collect();
        if players.is_empty() || defenses.is_empty() {
            continue;
        }
        stats.insert(week, WeekStats { players, defenses });
    }
    Ok(stats)
}

/// Score one fantasy team's entries: each contest that resolves to a week
/// with loaded stats gets a total; the rest are skipped.
pub fn score_entries(
    entries: &[FantasyTeamEntry],
    contest_week: &HashMap<ContestId, Week>,
    stats: &HashMap<Week, WeekStats>,
) -> Vec<(ContestId, f64)> {
    entries
        .iter()
        .filter_map(|e| {
            let week_stats = stats.get(contest_week.get(&e.contest)?)?;
            Some((e.contest, scoring::score_lineup(&e.lineup, week_stats)))
        })
        .collect()
}
