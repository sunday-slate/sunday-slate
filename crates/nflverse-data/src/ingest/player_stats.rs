use serde::Deserialize;

use crate::error::NflDataError;
use crate::ingest::{count, parse_csv, parse_season_type, yards};
use crate::model::{PlayerWeekStats, Season, TeamAbbr, Week};

/// nflverse stats_player_week_{season}.csv columns (subset we map). Counting
/// stats are Option<f64>: upstream writes NA as an empty cell, meaning 0.
#[derive(Debug, Deserialize)]
struct RawPlayerWeek {
    player_id: String,
    season: u16,
    week: u8,
    season_type: String,
    team: String,
    opponent_team: Option<String>,
    completions: Option<f64>,
    attempts: Option<f64>,
    passing_yards: Option<f64>,
    passing_tds: Option<f64>,
    passing_interceptions: Option<f64>,
    passing_2pt_conversions: Option<f64>,
    carries: Option<f64>,
    rushing_yards: Option<f64>,
    rushing_tds: Option<f64>,
    rushing_fumbles_lost: Option<f64>,
    rushing_2pt_conversions: Option<f64>,
    receptions: Option<f64>,
    targets: Option<f64>,
    receiving_yards: Option<f64>,
    receiving_tds: Option<f64>,
    receiving_fumbles_lost: Option<f64>,
    receiving_2pt_conversions: Option<f64>,
    sack_fumbles_lost: Option<f64>,
    special_teams_tds: Option<f64>,
    fumble_recovery_tds: Option<f64>,
    fg_made_0_19: Option<f64>,
    fg_made_20_29: Option<f64>,
    fg_made_30_39: Option<f64>,
    fg_made_40_49: Option<f64>,
    fg_made_50_59: Option<f64>,
    #[serde(rename = "fg_made_60_")]
    fg_made_60_plus: Option<f64>,
    fg_missed: Option<f64>,
    pat_made: Option<f64>,
    pat_missed: Option<f64>,
    fantasy_points: Option<f64>,
    fantasy_points_ppr: Option<f64>,
}

pub(crate) fn parse(asset: &str, bytes: &[u8]) -> Result<Vec<PlayerWeekStats>, NflDataError> {
    parse_csv(asset, bytes, |raw: RawPlayerWeek| {
        let season_type = parse_season_type(&raw.season_type)?;
        Ok(Some(PlayerWeekStats {
            season: Season(raw.season),
            week: Week(raw.week),
            season_type,
            gsis_id: raw.player_id,
            team: TeamAbbr(raw.team),
            opponent: raw.opponent_team.map(TeamAbbr),
            completions: count(raw.completions),
            attempts: count(raw.attempts),
            passing_yards: yards(raw.passing_yards),
            passing_tds: count(raw.passing_tds),
            passing_interceptions: count(raw.passing_interceptions),
            rushing_attempts: count(raw.carries),
            rushing_yards: yards(raw.rushing_yards),
            rushing_tds: count(raw.rushing_tds),
            targets: count(raw.targets),
            receptions: count(raw.receptions),
            receiving_yards: yards(raw.receiving_yards),
            receiving_tds: count(raw.receiving_tds),
            // saturating_add: three huge-but-valid f64 cells (e.g. malformed
            // upstream data) could each round to u32::MAX in count(); a plain
            // sum would panic (debug) or wrap (release) instead of just
            // saturating at a nonsense-but-safe value.
            fumbles_lost: count(raw.rushing_fumbles_lost)
                .saturating_add(count(raw.receiving_fumbles_lost))
                .saturating_add(count(raw.sack_fumbles_lost)),
            two_point_conversions: count(raw.passing_2pt_conversions)
                .saturating_add(count(raw.rushing_2pt_conversions))
                .saturating_add(count(raw.receiving_2pt_conversions)),
            special_teams_tds: count(raw.special_teams_tds),
            fumble_recovery_tds: count(raw.fumble_recovery_tds),
            fg_made_0_19: count(raw.fg_made_0_19),
            fg_made_20_29: count(raw.fg_made_20_29),
            fg_made_30_39: count(raw.fg_made_30_39),
            fg_made_40_49: count(raw.fg_made_40_49),
            fg_made_50_59: count(raw.fg_made_50_59),
            fg_made_60_plus: count(raw.fg_made_60_plus),
            fg_missed: count(raw.fg_missed),
            pat_made: count(raw.pat_made),
            pat_missed: count(raw.pat_missed),
            fantasy_points: raw.fantasy_points.unwrap_or(0.0),
            fantasy_points_ppr: raw.fantasy_points_ppr.unwrap_or(0.0),
        }))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::SeasonType;

    // Header + rows verbatim from nflverse stats_player_week_2025.csv (2026-07-01):
    // Aaron Rodgers week 1 (passing) and Matt Prater week 1 (kicking).
    const STATS_CSV: &str = include_str!("../../tests/fixtures/stats_player_week_2025.csv");

    #[test]
    fn maps_a_passing_line() {
        let stats = parse("stats_player_week_2025.csv", STATS_CSV.as_bytes()).unwrap();
        let rodgers = stats.iter().find(|s| s.gsis_id == "00-0023459").unwrap();

        assert_eq!(rodgers.season, Season(2025));
        assert_eq!(rodgers.week, Week(1));
        assert_eq!(rodgers.season_type, SeasonType::Reg);
        assert_eq!(rodgers.team, TeamAbbr("PIT".into()));
        assert_eq!(rodgers.opponent, Some(TeamAbbr("NYJ".into())));
        assert_eq!(rodgers.completions, 22);
        assert_eq!(rodgers.attempts, 30);
        assert_eq!(rodgers.passing_yards, 244);
        assert_eq!(rodgers.passing_tds, 4);
        assert_eq!(rodgers.passing_interceptions, 0);
        assert_eq!(rodgers.rushing_attempts, 1);
        assert_eq!(rodgers.rushing_yards, -1);
        assert_eq!(rodgers.fumbles_lost, 0);
        assert_eq!(rodgers.fantasy_points, 25.66);
        assert_eq!(rodgers.fantasy_points_ppr, 25.66);
    }

    #[test]
    fn maps_a_kicking_line_and_offense_only_fantasy_points() {
        let stats = parse("stats_player_week_2025.csv", STATS_CSV.as_bytes()).unwrap();
        let prater = stats.iter().find(|s| s.gsis_id == "00-0023853").unwrap();

        assert_eq!(prater.team, TeamAbbr("BUF".into()));
        assert_eq!(prater.fg_made_0_19, 0);
        assert_eq!(prater.fg_made_20_29, 1);
        assert_eq!(prater.fg_made_30_39, 1);
        assert_eq!(prater.fg_made_40_49, 1);
        assert_eq!(prater.fg_made_50_59, 0);
        assert_eq!(prater.fg_made_60_plus, 0);
        assert_eq!(prater.fg_missed, 0);
        assert_eq!(prater.pat_made, 2);
        assert_eq!(prater.pat_missed, 0);
        assert_eq!(prater.fantasy_points, 0.0);
    }

    #[test]
    fn garbage_in_a_numeric_column_fails_the_asset() {
        let csv = "player_id,season,week,season_type,team,opponent_team,passing_yards\n\
00-0000001,2025,1,REG,GB,CHI,plenty\n";
        let err = parse("stats_player_week_2025.csv", csv.as_bytes()).unwrap_err();
        assert!(err.to_string().contains("row 2"), "{err}");
    }

    #[test]
    fn overflowing_fumble_count_saturates_instead_of_panicking() {
        let csv = "player_id,season,week,season_type,team,opponent_team,\
rushing_fumbles_lost,receiving_fumbles_lost,sack_fumbles_lost,\
passing_2pt_conversions,rushing_2pt_conversions,receiving_2pt_conversions\n\
00-0000001,2025,1,REG,GB,CHI,\
99999999999999999999,99999999999999999999,99999999999999999999,\
99999999999999999999,99999999999999999999,99999999999999999999\n";
        let stats = parse("stats_player_week_2025.csv", csv.as_bytes()).unwrap();
        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0].fumbles_lost, u32::MAX);
        assert_eq!(stats[0].two_point_conversions, u32::MAX);
    }
}
