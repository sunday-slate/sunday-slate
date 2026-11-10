use sqlx::SqlitePool;

use crate::error::NflDataError;
use crate::model::{PlayerWeekStats, Season, SeasonType, SeasonWeek, TeamAbbr, Week};
use crate::store::Store;
use crate::store::sync_state::{self, SyncedAsset};

/// One `player_week_stats` row exactly as stored: the identity is decomposed
/// into its columns and the classification is a stored column. The domain type
/// carries a single [`SeasonWeek`] instead, so reads go through this adapter,
/// which validates the two against each other.
struct Row {
    season: Season,
    week: Week,
    season_type: SeasonType,
    gsis_id: String,
    team: TeamAbbr,
    opponent: Option<TeamAbbr>,
    completions: u32,
    attempts: u32,
    passing_yards: i32,
    passing_tds: u32,
    passing_interceptions: u32,
    rushing_attempts: u32,
    rushing_yards: i32,
    rushing_tds: u32,
    targets: u32,
    receptions: u32,
    receiving_yards: i32,
    receiving_tds: u32,
    fumbles_lost: u32,
    two_point_conversions: u32,
    special_teams_tds: u32,
    fumble_recovery_tds: u32,
}

fn decode_error(message: String) -> sqlx::Error {
    sqlx::Error::Decode(message.into())
}

impl TryFrom<Row> for PlayerWeekStats {
    type Error = sqlx::Error;

    fn try_from(row: Row) -> Result<Self, Self::Error> {
        let season_week = SeasonWeek::new(row.season, row.week)
            .ok_or_else(|| decode_error(format!("invalid absolute week {}", row.week.0)))?;
        let implied = if season_week.postseason() {
            SeasonType::Post
        } else {
            SeasonType::Reg
        };
        if row.season_type != implied {
            return Err(decode_error(format!(
                "season type {:?} conflicts with absolute week {}",
                row.season_type, season_week.week.0
            )));
        }
        Ok(Self {
            season_week,
            gsis_id: row.gsis_id,
            team: row.team,
            opponent: row.opponent,
            completions: row.completions,
            attempts: row.attempts,
            passing_yards: row.passing_yards,
            passing_tds: row.passing_tds,
            passing_interceptions: row.passing_interceptions,
            rushing_attempts: row.rushing_attempts,
            rushing_yards: row.rushing_yards,
            rushing_tds: row.rushing_tds,
            targets: row.targets,
            receptions: row.receptions,
            receiving_yards: row.receiving_yards,
            receiving_tds: row.receiving_tds,
            fumbles_lost: row.fumbles_lost,
            two_point_conversions: row.two_point_conversions,
            special_teams_tds: row.special_teams_tds,
            fumble_recovery_tds: row.fumble_recovery_tds,
        })
    }
}

pub(crate) async fn replace(
    store: &Store,
    asset: &SyncedAsset<'_>,
    season: Season,
    stats: &[PlayerWeekStats],
) -> Result<u64, NflDataError> {
    for s in stats {
        debug_assert_eq!(
            s.season_week.season, season,
            "player week stats entry season must match the replace() season param"
        );
    }

    store
        .write_tx(async |conn| {
            sqlx::query!("DELETE FROM player_week_stats WHERE season = ?", season)
                .execute(&mut *conn)
                .await?;
            for s in stats {
                let season_type = if s.season_week.postseason() {
                    SeasonType::Post
                } else {
                    SeasonType::Reg
                };
                sqlx::query!(
                    r#"INSERT INTO player_week_stats
                       (season, week, season_type, gsis_id, team, opponent,
                        completions, attempts,
                        passing_yards, passing_tds, passing_interceptions,
                        rushing_attempts, rushing_yards, rushing_tds,
                        targets, receptions, receiving_yards, receiving_tds,
                        fumbles_lost, two_point_conversions, special_teams_tds,
                        fumble_recovery_tds)
                       VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?,
                               ?, ?, ?, ?, ?)"#,
                    s.season_week.season,
                    s.season_week.week,
                    season_type,
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
    season_week: SeasonWeek,
) -> Result<Vec<PlayerWeekStats>, NflDataError> {
    let rows = sqlx::query_as!(
        Row,
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
               fumble_recovery_tds AS "fumble_recovery_tds: u32"
           FROM player_week_stats
           WHERE season = ? AND week = ?
           ORDER BY gsis_id, season_type"#,
        season_week.season,
        season_week.week
    )
    .fetch_all(pool)
    .await?;

    rows.into_iter()
        .map(PlayerWeekStats::try_from)
        .collect::<Result<Vec<_>, _>>()
        .map_err(NflDataError::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every numeric field gets a distinct value so that a positional
    /// argument swap in the INSERT's VALUES list would compile cleanly but
    /// flip two field values and fail the roundtrip assertion.
    fn stat_a() -> PlayerWeekStats {
        PlayerWeekStats {
            season_week: SeasonWeek::new(Season(2025), Week(1)).unwrap(),
            gsis_id: "00-1111111".into(),
            team: TeamAbbr::GB,
            opponent: Some(TeamAbbr::CHI),
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
        }
    }

    fn stat_b() -> PlayerWeekStats {
        PlayerWeekStats {
            season_week: SeasonWeek::new(Season(2025), Week(1)).unwrap(),
            gsis_id: "00-2222222".into(),
            team: TeamAbbr::DAL,
            opponent: Some(TeamAbbr::NYG),
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
        }
    }

    async fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&format!("sqlite://{}/t.db", dir.path().display()))
            .await
            .unwrap();
        (dir, store)
    }

    fn asset(name: &str) -> SyncedAsset<'_> {
        SyncedAsset {
            release_tag: "stats_player",
            asset_name: name,
            updated_at: "2026-06-20T00:00:00Z",
        }
    }

    #[tokio::test]
    async fn replace_then_for_week_roundtrips_every_field() {
        let (_dir, store) = store().await;
        let mut written = vec![stat_a(), stat_b()];
        replace(
            &store,
            &asset("stats_player_week_2025.csv"),
            Season(2025),
            &written,
        )
        .await
        .unwrap();

        let mut read_back = for_week(store.reader(), written[0].season_week)
            .await
            .unwrap();

        // for_week() orders by (gsis_id, season_type), so sorting `written`
        // the same way makes this a direct full-struct comparison rather
        // than just a length check.
        written.sort_by(|a, b| a.gsis_id.cmp(&b.gsis_id));
        read_back.sort_by(|a, b| a.gsis_id.cmp(&b.gsis_id));

        assert_eq!(read_back, written);
    }

    #[tokio::test]
    async fn season_week_roundtrips_postseason_weeks_in_full() {
        let (_dir, store) = store().await;
        let mut written = Vec::new();
        for week in [18u8, 19, 22] {
            let mut stat = stat_a();
            stat.season_week = SeasonWeek::new(Season(2025), Week(week)).unwrap();
            stat.gsis_id = format!("00-{week:07}");
            written.push(stat);
        }
        replace(
            &store,
            &asset("stats_player_week_2025.csv"),
            Season(2025),
            &written,
        )
        .await
        .unwrap();

        for expected in &written {
            let rows = for_week(store.reader(), expected.season_week)
                .await
                .unwrap();
            assert_eq!(rows, [expected.clone()]);
            assert_eq!(
                rows[0].season_week.postseason(),
                expected.season_week.week.0 > 18
            );
        }
    }

    #[tokio::test]
    async fn season_week_scopes_reads_to_the_season() {
        let (_dir, store) = store().await;
        let mut older = stat_a();
        older.season_week = SeasonWeek::new(Season(2024), Week(1)).unwrap();
        let mut newer = stat_b();
        newer.season_week = SeasonWeek::new(Season(2025), Week(1)).unwrap();
        replace(
            &store,
            &asset("stats_player_week_2024.csv"),
            Season(2024),
            std::slice::from_ref(&older),
        )
        .await
        .unwrap();
        replace(
            &store,
            &asset("stats_player_week_2025.csv"),
            Season(2025),
            std::slice::from_ref(&newer),
        )
        .await
        .unwrap();

        assert_eq!(
            for_week(store.reader(), newer.season_week).await.unwrap(),
            [newer]
        );
        assert_eq!(
            for_week(store.reader(), older.season_week).await.unwrap(),
            [older]
        );
    }

    #[tokio::test]
    async fn season_week_rejects_rows_whose_classification_disagrees() {
        let (_dir, store) = store().await;

        // season_type is stored as the enum's numeric repr in a TEXT column, so
        // '0' is what a regular-season row looks like on disk.
        sqlx::query(
            r#"INSERT INTO player_week_stats (season, week, season_type, gsis_id, team,
                   completions, attempts, passing_yards, passing_tds, passing_interceptions,
                   rushing_attempts, rushing_yards, rushing_tds, targets, receptions,
                   receiving_yards, receiving_tds, fumbles_lost, two_point_conversions,
                   special_teams_tds, fumble_recovery_tds)
               VALUES (2025, 19, '0', '00-1', 'GB', 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0)"#,
        )
        .execute(&store.write)
        .await
        .unwrap();
        let err = for_week(
            store.reader(),
            SeasonWeek::new(Season(2025), Week(19)).unwrap(),
        )
        .await;
        assert!(
            matches!(err, Err(NflDataError::Db(sqlx::Error::Decode(_)))),
            "{err:?}"
        );
    }
}
