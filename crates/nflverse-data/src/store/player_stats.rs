use sqlx::SqlitePool;

use crate::error::NflDataError;
use crate::model::{PlayerSeasonTotals, PlayerWeekStats, Season, SeasonType, TeamAbbr, Week};
use crate::store::Store;
use crate::store::sync_state::{self, SyncedAsset};

pub(crate) async fn replace(
    store: &Store,
    asset: &SyncedAsset<'_>,
    season: Season,
    stats: &[PlayerWeekStats],
) -> Result<u64, NflDataError> {
    for s in stats {
        debug_assert_eq!(
            s.season, season,
            "player week stats entry season must match the replace() season param"
        );
    }

    store
        .write_tx(async |conn| {
            sqlx::query!("DELETE FROM player_week_stats WHERE season = ?", season)
                .execute(&mut *conn)
                .await?;
            for s in stats {
                sqlx::query!(
                    r#"INSERT INTO player_week_stats
                       (season, week, season_type, gsis_id, team, opponent,
                        completions, attempts,
                        passing_yards, passing_tds, passing_interceptions,
                        rushing_attempts, rushing_yards, rushing_tds,
                        targets, receptions, receiving_yards, receiving_tds,
                        fumbles_lost, two_point_conversions, special_teams_tds,
                        fumble_recovery_tds,
                        fantasy_points, fantasy_points_ppr)
                       VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?,
                               ?, ?, ?, ?, ?, ?, ?)"#,
                    s.season,
                    s.week,
                    s.season_type,
                    s.gsis_id,
                    s.team,
                    s.opponent,
                    s.completions,
                    s.attempts,
                    s.passing_yards,
                    s.passing_tds,
                    s.passing_interceptions,
                    s.rushing_attempts,
                    s.rushing_yards,
                    s.rushing_tds,
                    s.targets,
                    s.receptions,
                    s.receiving_yards,
                    s.receiving_tds,
                    s.fumbles_lost,
                    s.two_point_conversions,
                    s.special_teams_tds,
                    s.fumble_recovery_tds,
                    s.fantasy_points,
                    s.fantasy_points_ppr
                )
                .execute(&mut *conn)
                .await?;
            }
            sync_state::record(conn, asset).await?;
            Ok(stats.len() as u64)
        })
        .await
}

pub(crate) async fn for_week(
    pool: &SqlitePool,
    season: Season,
    week: Week,
) -> Result<Vec<PlayerWeekStats>, NflDataError> {
    Ok(sqlx::query_as!(
        PlayerWeekStats,
        r#"SELECT
               season AS "season: Season",
               week AS "week: Week",
               season_type AS "season_type: SeasonType",
               gsis_id,
               team AS "team: TeamAbbr",
               opponent AS "opponent: TeamAbbr",
               completions AS "completions: u32",
               attempts AS "attempts: u32",
               passing_yards AS "passing_yards: i32",
               passing_tds AS "passing_tds: u32",
               passing_interceptions AS "passing_interceptions: u32",
               rushing_attempts AS "rushing_attempts: u32",
               rushing_yards AS "rushing_yards: i32",
               rushing_tds AS "rushing_tds: u32",
               targets AS "targets: u32",
               receptions AS "receptions: u32",
               receiving_yards AS "receiving_yards: i32",
               receiving_tds AS "receiving_tds: u32",
               fumbles_lost AS "fumbles_lost: u32",
               two_point_conversions AS "two_point_conversions: u32",
               special_teams_tds AS "special_teams_tds: u32",
               fumble_recovery_tds AS "fumble_recovery_tds: u32",
               fantasy_points AS "fantasy_points: f64",
               fantasy_points_ppr AS "fantasy_points_ppr: f64"
           FROM player_week_stats
           WHERE season = ? AND week = ?
           ORDER BY gsis_id, season_type"#,
        season,
        week
    )
    .fetch_all(pool)
    .await?)
}

pub(crate) async fn totals_before(
    pool: &SqlitePool,
    season: Season,
    before: Week,
) -> Result<Vec<PlayerSeasonTotals>, NflDataError> {
    Ok(sqlx::query_as!(
        PlayerSeasonTotals,
        r#"SELECT gsis_id,
                  SUM(fantasy_points_ppr) AS "fantasy_points_ppr!: f64",
                  COUNT(*) AS "games!: u32"
           FROM player_week_stats
           WHERE season = ? AND week < ?
           GROUP BY gsis_id"#,
        season,
        before
    )
    .fetch_all(pool)
    .await?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::SeasonType;

    /// Every numeric field gets a distinct value so that a positional
    /// argument swap in the INSERT's VALUES list would compile cleanly but
    /// flip two field values and fail the roundtrip assertion.
    fn stat_a() -> PlayerWeekStats {
        PlayerWeekStats {
            season: Season(2025),
            week: Week(1),
            season_type: SeasonType::Reg,
            gsis_id: "00-1111111".into(),
            team: TeamAbbr("GB".into()),
            opponent: Some(TeamAbbr("CHI".into())),
            completions: 199,
            attempts: 201,
            passing_yards: 233,
            passing_tds: 34,
            passing_interceptions: 12,
            rushing_attempts: 41,
            rushing_yards: -9,
            rushing_tds: 27,
            targets: 18,
            receptions: 15,
            receiving_yards: 71,
            receiving_tds: 6,
            fumbles_lost: 3,
            two_point_conversions: 2,
            special_teams_tds: 1,
            fumble_recovery_tds: 59,
            fantasy_points: 88.42,
            fantasy_points_ppr: 101.77,
        }
    }

    fn stat_b() -> PlayerWeekStats {
        PlayerWeekStats {
            season: Season(2025),
            week: Week(1),
            season_type: SeasonType::Reg,
            gsis_id: "00-2222222".into(),
            team: TeamAbbr("DAL".into()),
            opponent: Some(TeamAbbr("NYG".into())),
            completions: 202,
            attempts: 203,
            passing_yards: -14,
            passing_tds: 44,
            passing_interceptions: 4,
            rushing_attempts: 55,
            rushing_yards: 63,
            rushing_tds: 7,
            targets: 39,
            receptions: 28,
            receiving_yards: 17,
            receiving_tds: 5,
            fumbles_lost: 8,
            two_point_conversions: 6,
            special_teams_tds: 2,
            fumble_recovery_tds: 109,
            fantasy_points: 12.5,
            fantasy_points_ppr: 19.25,
        }
    }

    #[tokio::test]
    async fn replace_then_for_week_roundtrips_every_field() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&format!("sqlite://{}/t.db", dir.path().display()))
            .await
            .unwrap();
        let asset = SyncedAsset {
            release_tag: "stats_player",
            asset_name: "stats_player_week_2025.csv",
            updated_at: "2026-06-20T00:00:00Z",
        };

        let mut written = vec![stat_a(), stat_b()];
        replace(&store, &asset, Season(2025), &written)
            .await
            .unwrap();

        let mut read_back = for_week(store.reader(), Season(2025), Week(1))
            .await
            .unwrap();

        // for_week() orders by (gsis_id, season_type), so sorting `written`
        // the same way makes this a direct full-struct comparison rather
        // than just a length check.
        written.sort_by(|a, b| a.gsis_id.cmp(&b.gsis_id));
        read_back.sort_by(|a, b| a.gsis_id.cmp(&b.gsis_id));

        assert_eq!(read_back, written);
    }
}
