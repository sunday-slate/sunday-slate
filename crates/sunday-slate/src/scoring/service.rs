//! Scoring assembly a handler can call: load the stats a set of contests
//! needs, then score each fantasy team's entries against them.

use std::collections::{HashMap, HashSet};

use nfl_data::{NflData, NflDataError, Season, TeamAbbr, Week};

use crate::contests::ContestId;
use crate::entries::{FantasyTeamEntry, NflPlayerId};
use crate::scoring::{self, PointsPerGame, WeekStats};

/// Season-to-date FPPG and games played, keyed per position group: each
/// gsis id in `ids` under `players`, and every team's defense under `dsts`.
/// Both cover the weeks before `before`. Empty before week 2.
///
/// Every prior week's stat line is scored with the contest's own rules
/// ([`score_player`]: half-point receptions, yardage bonuses, and the rest;
/// [`score_defense`]: sacks, picks, safeties, and points-allowed tiers), so
/// the displayed pace and contest scoring agree. A line is scored on its
/// own week — the 300+ passing yards and 100+ rushing/receiving bonuses are
/// per-game, and so are the points-allowed tiers, so they must be applied
/// before averaging, never to a summed season line. Games counted are the
/// cached stat rows, matching the old upstream convention.
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
                        games_played: games,
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
                        games_played: games,
                    },
                )
            })
            .collect(),
    })
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scoring::PointsPerGame;
    use crate::tests::factories;
    use nfl_data::PlayerWeekStats;

    /// Two played weeks: 150 receiving yards in week 1 (15.0 + the 100+ ReY
    /// bonus = 18.0) and 40 in week 2 (4.0). Averaging per-week bonus-adjusted
    /// scores gives 11.00; summing before scoring would give 19.0 / 2 = 9.50.
    #[tokio::test]
    async fn fppg_scores_each_week_separately() {
        let nfl = nfl_data::NflData::in_memory().await.unwrap();
        let week1 = PlayerWeekStats {
            receiving_yards: 150,
            ..factories::player_stats("00-A", 1)
        };
        let week2 = PlayerWeekStats {
            receiving_yards: 40,
            ..factories::player_stats("00-A", 2)
        };
        let other = PlayerWeekStats {
            receiving_yards: 999,
            ..factories::player_stats("00-B", 1)
        };
        nfl.seed_week_stats_for_test(Season(2025), &[week1, week2, other], &[])
            .await
            .unwrap();

        let averages = season_averages(&nfl, Season(2025), &["00-A", "unknown"], Week(3))
            .await
            .unwrap();
        assert_eq!(
            averages.players[&NflPlayerId("00-A".into())],
            PointsPerGame {
                avg_points: 11.0,
                games_played: 2
            }
        );
        // Only requested ids come back, and an id with no stat rows is absent.
        assert_eq!(averages.players.len(), 1);
    }

    /// Defense pace applies each week's points-allowed tier before averaging:
    /// a 24-PA shutout with 3 sacks (3.0) and a scoreless week (10.0) average
    /// to 6.5 per game. Summing first would give 24 PA -> the 0.0 tier for the
    /// whole season line, then (0.0 + 3.0) / 2 = 1.5.
    #[tokio::test]
    async fn dst_pace_scores_each_week_separately() {
        let nfl = nfl_data::NflData::in_memory().await.unwrap();
        let mut shutout = factories::scoreless_defense_stats("KC", "BUF", 1);
        shutout.sacks = 3;
        let goose_egg = factories::defense_stats("KC", "LAC", 2);
        nfl.seed_week_stats_for_test(Season(2025), &[], &[shutout, goose_egg])
            .await
            .unwrap();

        let averages = season_averages(&nfl, Season(2025), &["00-A"], Week(3))
            .await
            .unwrap();
        // Shutout: 0 PA tier (0.0) + 3 sacks (3.0) = 3.0. Scoreless: 10.0.
        assert_eq!(
            averages.dsts[&"KC".parse().unwrap()],
            PointsPerGame {
                avg_points: 6.5,
                games_played: 2
            }
        );
    }

    /// A scoreless defense (24 points allowed, nothing else) totals 0.0 but
    /// still counts one game per played week: the chooser renders 0.0 and the
    /// game count, never dashes, when a stat row exists.
    #[tokio::test]
    async fn scoreless_defense_counts_toward_pace() {
        let nfl = nfl_data::NflData::in_memory().await.unwrap();
        nfl.seed_week_stats_for_test(
            Season(2025),
            &[],
            &[
                factories::scoreless_defense_stats("KC", "BUF", 1),
                factories::scoreless_defense_stats("KC", "LAC", 2),
            ],
        )
        .await
        .unwrap();

        let averages = season_averages(&nfl, Season(2025), &["00-A"], Week(3))
            .await
            .unwrap();
        assert_eq!(
            averages.dsts[&"KC".parse().unwrap()],
            PointsPerGame {
                avg_points: 0.0,
                games_played: 2
            }
        );
    }

    #[tokio::test]
    async fn no_weeks_played_means_no_averages() {
        let nfl = nfl_data::NflData::in_memory().await.unwrap();
        let stats = season_averages(&nfl, Season(2025), &["00-A"], Week(2))
            .await
            .unwrap();
        assert!(stats.players.is_empty());
        assert!(stats.dsts.is_empty());
    }
}
