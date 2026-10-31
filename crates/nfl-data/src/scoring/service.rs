//! Scoring assembly a handler can call: load the stats a set of contests
//! needs, then score each fantasy team's entries against them.

use crate::{NflData, NflDataError, Season, TeamAbbr, Week};
use nfl_model::NflPlayerId;
use std::collections::{HashMap, HashSet};

use crate::scoring::{self, PointsPerGame, WeekStats};

/// Season-to-date FPPG and games played for both NFL Players and Team D/ST
#[derive(Debug, Default)]
pub struct SeasonAverages {
    pub players: HashMap<NflPlayerId, PointsPerGame>,
    pub dsts: HashMap<TeamAbbr, PointsPerGame>,
}

pub async fn season_averages(
    nfl: &NflData,
    season: Season,
    ids: &[impl AsRef<str>],
    before: Week,
) -> Result<SeasonAverages, NflDataError> {
    let wanted: HashSet<&str> = ids.iter().map(|i| i.as_ref()).collect();
    let mut players: HashMap<NflPlayerId, (f64, u32)> = HashMap::new();
    let mut dsts: HashMap<TeamAbbr, (f64, u32)> = HashMap::new();
    for week in 1..before.0 {
        for line in nfl.player_week_stats(season, Week(week)).await? {
            if !wanted.contains(line.gsis_id.as_str()) {
                continue;
            }
            let (points, games) = players
                .entry(NflPlayerId(line.gsis_id.clone()))
                .or_insert_with(|| (0.0, 0));
            *points += scoring::score_player(&line).total;
            *games += 1;
        }
        for t in nfl.team_week_stats(season, Week(week)).await? {
            let (points, games) = dsts.entry(t.team).or_insert_with(|| (0.0, 0));
            *points += scoring::score_defense(&t).total;
            *games += 1;
        }
    }
    Ok(SeasonAverages {
        players: players
            .into_iter()
            .map(|(id, (points, games))| {
                (
                    id,
                    PointsPerGame {
                        avg_points: points / games as f64,
                        games_played: games as u8,
                    },
                )
            })
            .collect(),
        dsts: dsts
            .into_iter()
            .map(|(team, (points, games))| {
                (
                    team,
                    PointsPerGame {
                        avg_points: points / games as f64,
                        games_played: games as u8,
                    },
                )
            })
            .collect(),
    })
}

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
