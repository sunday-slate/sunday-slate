//! Scoring assembly a handler can call: load the stats a set of contests
//! needs, then score each fantasy team's entries against them.

use crate::{NflData, NflDataError, SeasonWeek, TeamAbbr, Week};
use nfl_model::NflPlayerId;
use std::collections::{HashMap, HashSet};

use crate::scoring::{self, PointsPerGame, WeekStats};

/// Season-to-date FPPG and games played for both NFL Players and Team DST
#[derive(Debug, Default)]
pub struct SeasonAverages {
    pub players: HashMap<NflPlayerId, PointsPerGame>,
    pub dsts: HashMap<TeamAbbr, PointsPerGame>,
}

/// Averages for every week of `before`'s season strictly before that week, so
/// the cutoff week and other seasons' rows never participate.
pub async fn season_averages(
    nfl: &NflData,
    before: SeasonWeek,
    ids: &[impl AsRef<str>],
) -> Result<SeasonAverages, NflDataError> {
    let wanted: HashSet<&str> = ids.iter().map(|i| i.as_ref()).collect();
    let mut players: HashMap<NflPlayerId, (f64, u32)> = HashMap::new();
    let mut dsts: HashMap<TeamAbbr, (f64, u32)> = HashMap::new();
    for week in 1..before.week.0 {
        let season_week = SeasonWeek {
            season: before.season,
            week: Week(week),
        };
        for line in nfl.player_week_stats(season_week).await? {
            if !wanted.contains(line.gsis_id.as_str()) {
                continue;
            }
            let (points, games) = players
                .entry(NflPlayerId(line.gsis_id.clone()))
                .or_insert_with(|| (0.0, 0));
            *points += scoring::score_player(&line).total;
            *games += 1;
        }
        for t in nfl.team_week_stats(season_week).await? {
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
    weeks: impl IntoIterator<Item = SeasonWeek>,
) -> Result<HashMap<SeasonWeek, WeekStats>, NflDataError> {
    let mut stats: HashMap<SeasonWeek, WeekStats> = HashMap::new();
    let mut seen: HashSet<SeasonWeek> = HashSet::new();
    for week in weeks {
        if !seen.insert(week) {
            continue;
        }
        let players: HashMap<NflPlayerId, _> = nfl
            .player_week_stats(week)
            .await?
            .into_iter()
            .map(|p| (NflPlayerId(p.gsis_id.clone()), p))
            .collect();
        let defenses: HashMap<_, _> = nfl
            .team_week_stats(week)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PlayerWeekStats;
    use crate::{NflData, Season, SeasonWeek, TeamWeekStats};

    fn week(season: u16, week: u8) -> SeasonWeek {
        SeasonWeek::new(Season(season), Week(week)).expect("valid fixture week")
    }

    /// Every scoring count zero except receptions, so the total is exactly
    /// `0.5 * receptions` with no threshold bonus in play.
    fn receptions(season_week: SeasonWeek, receptions: u32) -> PlayerWeekStats {
        PlayerWeekStats {
            season_week,
            gsis_id: "00-RB".into(),
            team: TeamAbbr::KC,
            opponent: Some(TeamAbbr::BUF),
            completions: 0,
            attempts: 0,
            passing_yards: 0,
            passing_tds: 0,
            passing_interceptions: 0,
            rushing_attempts: 0,
            rushing_yards: 0,
            rushing_tds: 0,
            targets: 0,
            receptions,
            receiving_yards: 0,
            receiving_tds: 0,
            fumbles_lost: 0,
            two_point_conversions: 0,
            special_teams_tds: 0,
            fumble_recovery_tds: 0,
        }
    }

    fn defense(season_week: SeasonWeek, points_allowed: i32) -> TeamWeekStats {
        TeamWeekStats {
            season_week,
            team: TeamAbbr::KC,
            opponent: TeamAbbr::BUF,
            game_id: format!("{}_{:02}_BUF_KC", season_week.season, season_week.week.0),
            sacks: 0,
            interceptions: 0,
            fumble_recoveries: 0,
            safeties: 0,
            touchdowns: 0,
            blocked_kicks: 0,
            conversion_returns: 0,
            points_allowed,
        }
    }

    async fn seeded() -> NflData {
        let nfl = NflData::in_memory().await.expect("in-memory handle");
        // 2025 week 1: 20 receptions = 10.00, a shutout = 10.00.
        nfl.seed_week_stats_for_test(
            Season(2025),
            &[receptions(week(2025, 1), 20)],
            &[defense(week(2025, 1), 0)],
        )
        .await
        .expect("seed 2025");
        // 2026 week 1: 40 receptions = 20.00, 7 points allowed = 4.00.
        nfl.seed_week_stats_for_test(
            Season(2026),
            &[receptions(week(2026, 1), 40)],
            &[defense(week(2026, 1), 7)],
        )
        .await
        .expect("seed 2026");
        nfl
    }

    #[tokio::test]
    async fn season_week_week_stats_keys_each_season_separately() {
        let nfl = seeded().await;
        let stats = week_stats(
            &nfl,
            [week(2025, 1), week(2026, 1), week(2025, 1), week(2025, 1)],
        )
        .await
        .expect("load stats");

        assert_eq!(stats.len(), 2, "the duplicate identity collapses");
        let id = NflPlayerId("00-RB".into());

        let older = &stats[&week(2025, 1)];
        assert_eq!(scoring::score_player(&older.players[&id]).total, 10.0);
        assert_eq!(
            scoring::score_defense(&older.defenses[&TeamAbbr::KC]).total,
            10.0
        );

        let newer = &stats[&week(2026, 1)];
        assert_eq!(scoring::score_player(&newer.players[&id]).total, 20.0);
        assert_eq!(
            scoring::score_defense(&newer.defenses[&TeamAbbr::KC]).total,
            4.0
        );
    }

    #[tokio::test]
    async fn season_week_week_stats_skips_a_week_without_defenses() {
        let nfl = NflData::in_memory().await.expect("in-memory handle");
        nfl.seed_week_stats_for_test(Season(2025), &[receptions(week(2025, 2), 20)], &[])
            .await
            .expect("seed players only");

        assert!(
            week_stats(&nfl, [week(2025, 2)])
                .await
                .expect("load stats")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn season_week_season_averages_stop_before_the_cutoff_week() {
        let nfl = NflData::in_memory().await.expect("in-memory handle");
        // Prior weeks: 20 receptions (10.00) and 40 receptions (20.00).
        // The cutoff week and the other season's week 18 must not count.
        nfl.seed_week_stats_for_test(
            Season(2025),
            &[
                receptions(week(2025, 18), 20),
                receptions(week(2025, 19), 40),
                receptions(week(2025, 20), 900),
            ],
            &[
                defense(week(2025, 18), 0),
                defense(week(2025, 19), 0),
                defense(week(2025, 20), 0),
            ],
        )
        .await
        .expect("seed 2025");
        nfl.seed_week_stats_for_test(
            Season(2026),
            &[receptions(week(2026, 18), 800)],
            &[defense(week(2026, 18), 0)],
        )
        .await
        .expect("seed 2026");

        let averages = season_averages(&nfl, week(2025, 20), &["00-RB"])
            .await
            .expect("averages");
        let player = averages.players[&NflPlayerId("00-RB".into())];
        assert_eq!(player.games_played, 2);
        assert_eq!(player.avg_points, 15.0);
        // Both DST weeks allowed a shutout: 10.00 each.
        let dst = averages.dsts[&TeamAbbr::KC];
        assert_eq!(dst.games_played, 2);
        assert_eq!(dst.avg_points, 10.0);
    }

    #[tokio::test]
    async fn season_week_season_averages_of_week_one_are_empty() {
        let nfl = seeded().await;
        let averages = season_averages(&nfl, week(2025, 1), &["00-RB"])
            .await
            .expect("averages");
        assert!(averages.players.is_empty());
        assert!(averages.dsts.is_empty());
    }
}
