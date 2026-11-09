//! Scoring assembly a handler can call: load the stats a set of contests
//! needs, then score each fantasy team's entries against them.

use std::collections::{HashMap, HashSet};

use nfl_data::{NflData, NflDataError, Season, Week};

use crate::entries::{FantasyTeamEntry, NflPlayerId};
use crate::scoring::{self, WeekStats};
use nfl_data::slates::SlateId;

/// Load all fantasy stats for completed weeks.
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
            .map(|t| (t.team, t))
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
    contest_week: &HashMap<SlateId, Week>,
    stats: &HashMap<Week, WeekStats>,
) -> Vec<(SlateId, f64)> {
    entries
        .iter()
        .filter_map(|e| {
            let week_stats = stats.get(contest_week.get(&e.slate_id)?)?;
            Some((e.slate_id, scoring::score_lineup(&e.lineup, week_stats)))
        })
        .collect()
}
