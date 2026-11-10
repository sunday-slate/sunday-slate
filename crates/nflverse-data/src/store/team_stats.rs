use sqlx::SqlitePool;

use crate::error::NflDataError;
use crate::model::{Season, SeasonType, SeasonWeek, TeamAbbr, TeamWeekStats, Week};
use crate::store::Store;
use crate::store::sync_state::{self, SyncedAsset};

/// One `team_week_stats` row exactly as stored: decomposed identity plus the
/// stored classification. The domain type carries a single [`SeasonWeek`], so
/// reads go through this adapter, which validates the two against each other.
struct Row {
    season: Season,
    week: Week,
    season_type: SeasonType,
    team: TeamAbbr,
    opponent: TeamAbbr,
    game_id: String,
    sacks: u32,
    interceptions: u32,
    fumble_recoveries: u32,
    safeties: u32,
    touchdowns: u32,
    blocked_kicks: u32,
    conversion_returns: u32,
    points_allowed: i32,
}

fn decode_error(message: String) -> sqlx::Error {
    sqlx::Error::Decode(message.into())
}

impl TryFrom<Row> for TeamWeekStats {
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
            team: row.team,
            opponent: row.opponent,
            game_id: row.game_id,
            sacks: row.sacks,
            interceptions: row.interceptions,
            fumble_recoveries: row.fumble_recoveries,
            safeties: row.safeties,
            touchdowns: row.touchdowns,
            blocked_kicks: row.blocked_kicks,
            conversion_returns: row.conversion_returns,
            points_allowed: row.points_allowed,
        })
    }
}

pub(crate) async fn replace(
    store: &Store,
    asset: &SyncedAsset<'_>,
    season: Season,
    stats: &[TeamWeekStats],
) -> Result<u64, NflDataError> {
    for s in stats {
        debug_assert_eq!(
            s.season_week.season, season,
            "team week stats entry season must match the replace() season param"
        );
    }

    store
        .write_tx(async |conn| {
            sqlx::query!("DELETE FROM team_week_stats WHERE season = ?", season)
                .execute(&mut *conn)
                .await?;
            for s in stats {
                let season_type = if s.season_week.postseason() {
                    SeasonType::Post
                } else {
                    SeasonType::Reg
                };
                sqlx::query!(
                    r#"INSERT INTO team_week_stats
                       (season, week, season_type, team, opponent, game_id,
                        sacks, interceptions, fumble_recoveries, safeties,
                        touchdowns, blocked_kicks, conversion_returns, points_allowed)
                       VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
                    s.season_week.season,
                    s.season_week.week,
                    season_type,
                    s.team,
                    s.opponent,
                    s.game_id,
                    s.sacks,
                    s.interceptions,
                    s.fumble_recoveries,
                    s.safeties,
                    s.touchdowns,
                    s.blocked_kicks,
                    s.conversion_returns,
                    s.points_allowed,
                )
                .execute(&mut *conn)
                .await?;
            }
            sync_state::record(conn, asset).await?;
            Ok(stats.len() as u64)
        })
        .await
}

/// One row per team for the week, read straight from the pbp-derived table —
/// opponent and points_allowed were computed at ingest, so no joins.
pub(crate) async fn for_week(
    pool: &SqlitePool,
    season_week: SeasonWeek,
) -> Result<Vec<TeamWeekStats>, NflDataError> {
    let rows = sqlx::query_as!(
        Row,
        r#"SELECT
               season AS "season: Season",
               week AS "week: Week",
               season_type AS "season_type: SeasonType",
               team AS "team: TeamAbbr",
               opponent AS "opponent: TeamAbbr",
               game_id,
               sacks AS "sacks: u32",
               interceptions AS "interceptions: u32",
               fumble_recoveries AS "fumble_recoveries: u32",
               safeties AS "safeties: u32",
               touchdowns AS "touchdowns: u32",
               blocked_kicks AS "blocked_kicks: u32",
               conversion_returns AS "conversion_returns: u32",
               points_allowed AS "points_allowed: i32"
           FROM team_week_stats
           WHERE season = ? AND week = ?
           ORDER BY team, season_type"#,
        season_week.season,
        season_week.week
    )
    .fetch_all(pool)
    .await?;

    rows.into_iter()
        .map(TeamWeekStats::try_from)
        .collect::<Result<Vec<_>, _>>()
        .map_err(NflDataError::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::sync_state::SyncedAsset;

    fn row(team: &str, opp: &str) -> TeamWeekStats {
        // Distinct primes so a positional INSERT swap fails the roundtrip.
        TeamWeekStats {
            season_week: SeasonWeek::new(Season(2025), Week(1)).unwrap(),
            team: team.parse().unwrap(),
            opponent: opp.parse().unwrap(),
            game_id: "2025_01_AAA_BBB".into(),
            sacks: 3,
            interceptions: 5,
            fumble_recoveries: 7,
            safeties: 11,
            touchdowns: 13,
            blocked_kicks: 17,
            conversion_returns: 19,
            points_allowed: 23,
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
            release_tag: "pbp",
            asset_name: name,
            updated_at: "2026-06-30T10:36:16Z",
        }
    }

    #[tokio::test]
    async fn replace_then_for_week_roundtrips() {
        let (_dir, store) = store().await;
        let aaa = row("KC", "BUF");
        replace(
            &store,
            &asset("play_by_play_2025.csv"),
            Season(2025),
            std::slice::from_ref(&aaa),
        )
        .await
        .unwrap();
        let back = for_week(store.reader(), aaa.season_week).await.unwrap();
        assert_eq!(back, vec![aaa]);
    }

    #[tokio::test]
    async fn season_week_roundtrips_postseason_weeks_in_full() {
        let (_dir, store) = store().await;
        let mut written = Vec::new();
        for week in [18u8, 19, 22] {
            let mut stats = row("KC", "BUF");
            stats.season_week = SeasonWeek::new(Season(2025), Week(week)).unwrap();
            stats.game_id = format!("2025_{week:02}_BUF_KC");
            written.push(stats);
        }
        replace(
            &store,
            &asset("play_by_play_2025.csv"),
            Season(2025),
            &written,
        )
        .await
        .unwrap();

        for expected in &written {
            let rows = for_week(store.reader(), expected.season_week)
                .await
                .unwrap();
            assert_eq!(rows, std::slice::from_ref(expected));
            assert_eq!(
                rows[0].season_week.postseason(),
                expected.season_week.week.0 > 18
            );
        }
    }

    #[tokio::test]
    async fn season_week_scopes_reads_to_the_season() {
        let (_dir, store) = store().await;
        let mut older = row("KC", "BUF");
        older.season_week = SeasonWeek::new(Season(2024), Week(1)).unwrap();
        let mut newer = row("KC", "BUF");
        newer.season_week = SeasonWeek::new(Season(2025), Week(1)).unwrap();
        newer.points_allowed = 99;
        replace(
            &store,
            &asset("play_by_play_2024.csv"),
            Season(2024),
            std::slice::from_ref(&older),
        )
        .await
        .unwrap();
        replace(
            &store,
            &asset("play_by_play_2025.csv"),
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
        // '1' is what a postseason row looks like on disk.
        sqlx::query(
            r#"INSERT INTO team_week_stats (season, week, season_type, team, opponent, game_id,
                   sacks, interceptions, fumble_recoveries, safeties, touchdowns,
                   blocked_kicks, conversion_returns, points_allowed)
               VALUES (2025, 18, '1', 'KC', 'BUF', '2025_18_BUF_KC', 0, 0, 0, 0, 0, 0, 0, 0)"#,
        )
        .execute(&store.write)
        .await
        .unwrap();

        let err = for_week(
            store.reader(),
            SeasonWeek::new(Season(2025), Week(18)).unwrap(),
        )
        .await;
        assert!(
            matches!(err, Err(NflDataError::Db(sqlx::Error::Decode(_)))),
            "{err:?}"
        );
    }
}
