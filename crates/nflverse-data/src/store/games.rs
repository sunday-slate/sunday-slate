use crate::error::NflDataError;
use crate::model::{Game, Season, SeasonType, SeasonWeek};
use crate::store::Store;
use crate::store::sync_state::{self, SyncedAsset};
use sqlx::{Row, SqliteConnection, SqlitePool};

/// Insert games directly, bypassing the sync pipeline and `sync_state`
/// bookkeeping. Runs inside a caller-supplied transaction. For tests and
/// tooling only — see `NflData::seed_for_test`.
pub(crate) async fn seed(conn: &mut SqliteConnection, games: &[Game]) -> Result<(), NflDataError> {
    for g in games {
        let season_type = if g.season_week.postseason() {
            SeasonType::Post
        } else {
            SeasonType::Reg
        };
        sqlx::query!(
            r#"INSERT INTO games
               (game_id, season, week, season_type, kickoff,
                home_team, away_team, home_score, away_score)
               VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
            g.game_id,
            g.season_week.season,
            g.season_week.week,
            season_type,
            g.kickoff,
            g.home_team,
            g.away_team,
            g.home_score,
            g.away_score
        )
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}

pub(crate) async fn replace(
    store: &Store,
    asset: &SyncedAsset<'_>,
    games: &[Game],
) -> Result<u64, NflDataError> {
    store
        .write_tx(async |conn| {
            sqlx::query!("DELETE FROM games")
                .execute(&mut *conn)
                .await?;
            for g in games {
                let season_type = if g.season_week.postseason() {
                    SeasonType::Post
                } else {
                    SeasonType::Reg
                };
                sqlx::query!(
                    r#"INSERT INTO games
                       (game_id, season, week, season_type, kickoff,
                        home_team, away_team, home_score, away_score)
                       VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
                    g.game_id,
                    g.season_week.season,
                    g.season_week.week,
                    season_type,
                    g.kickoff,
                    g.home_team,
                    g.away_team,
                    g.home_score,
                    g.away_score
                )
                .execute(&mut *conn)
                .await?;
            }
            sync_state::record(conn, asset).await?;
            Ok(games.len() as u64)
        })
        .await
}

pub(crate) async fn for_season(
    pool: &SqlitePool,
    season: Season,
) -> Result<Vec<Game>, NflDataError> {
    Ok(sqlx::query(
        r#"SELECT
               game_id,
               season,
               week,
               kickoff,
               home_team,
               away_team,
               home_score,
               away_score
           FROM games
           WHERE season = ?
           ORDER BY week, game_id"#,
    )
    .bind(season)
    .try_map(|row| {
        Ok(Game {
            game_id: row.try_get("game_id")?,
            season_week: SeasonWeek {
                season: row.try_get("season")?,
                week: row.try_get("week")?,
            },
            kickoff: row.try_get("kickoff")?,
            home_team: row.try_get("home_team")?,
            away_team: row.try_get("away_team")?,
            home_score: row.try_get("home_score")?,
            away_score: row.try_get("away_score")?,
        })
    })
    .fetch_all(pool)
    .await?)
}

#[cfg(test)]
mod tests {
    use crate::model::{TeamAbbr, Week};
    use time::macros::datetime;

    use super::*;

    fn game(id: &str, week: u8) -> Game {
        Game {
            game_id: id.to_string(),
            season_week: SeasonWeek {
                season: Season(2025),
                week: Week(week),
            },
            kickoff: Some(datetime!(2025-10-03 00:15 UTC)),
            home_team: TeamAbbr::LA,
            away_team: TeamAbbr::SF,
            home_score: Some(23),
            away_score: Some(26),
        }
    }

    #[tokio::test]
    async fn replace_roundtrips_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&format!("sqlite://{}/t.db", dir.path().display()))
            .await
            .unwrap();
        let asset = SyncedAsset {
            release_tag: "schedules",
            asset_name: "games.csv",
            updated_at: "2026-06-30T10:36:16Z",
        };

        let n = replace(&store, &asset, &[game("2025_05_SF_LA", 5)])
            .await
            .unwrap();
        assert_eq!(n, 1);
        assert!(
            sync_state::is_current(
                store.reader(),
                "schedules",
                "games.csv",
                "2026-06-30T10:36:16Z"
            )
            .await
            .unwrap()
        );
        assert!(
            !sync_state::is_current(store.reader(), "schedules", "games.csv", "different")
                .await
                .unwrap()
        );

        // Replacing again fully supersedes the old contents.
        replace(&store, &asset, &[game("2025_06_SF_LA", 6)])
            .await
            .unwrap();
        let games = for_season(store.reader(), Season(2025)).await.unwrap();
        assert_eq!(games.len(), 1);
        assert_eq!(games[0].game_id, "2025_06_SF_LA");
        assert_eq!(games[0].kickoff, Some(datetime!(2025-10-03 00:15 UTC)));
    }

    #[tokio::test]
    async fn for_season_round_trips_canonical_week_18_and_week_22() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&format!("sqlite://{}/t.db", dir.path().display()))
            .await
            .unwrap();

        store
            .write_tx(async |conn| {
                seed(
                    conn,
                    &[game("2025_18_SF_LA", 18), game("2025_22_SEA_NE", 22)],
                )
                .await?;
                Ok(())
            })
            .await
            .unwrap();

        let games = for_season(store.reader(), Season(2025)).await.unwrap();
        assert_eq!(games.len(), 2);

        assert_eq!(games[0].game_id, "2025_18_SF_LA");
        assert_eq!(games[0].season_week.season, Season(2025));
        assert_eq!(games[0].season_week.week, Week(18));
        assert!(!games[0].season_week.postseason());

        assert_eq!(games[1].game_id, "2025_22_SEA_NE");
        assert_eq!(games[1].season_week.season, Season(2025));
        assert_eq!(games[1].season_week.week, Week(22));
        assert!(games[1].season_week.postseason());

        // The stored classification column stays consistent with the absolute week.
        let stored: Vec<(String, i64)> = sqlx::query_as(
            "SELECT game_id, CAST(season_type AS INTEGER) FROM games ORDER BY game_id",
        )
        .fetch_all(store.reader())
        .await
        .unwrap();
        assert_eq!(stored.len(), 2);
        assert_eq!(stored[0], ("2025_18_SF_LA".to_string(), 0));
        assert_eq!(stored[1], ("2025_22_SEA_NE".to_string(), 1));
    }

    #[tokio::test]
    async fn replace_rolls_back_and_leaves_previous_data_intact_on_failure() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&format!("sqlite://{}/t.db", dir.path().display()))
            .await
            .unwrap();
        let asset_a = SyncedAsset {
            release_tag: "schedules",
            asset_name: "games.csv",
            updated_at: "2026-06-30T10:36:16Z",
        };

        replace(&store, &asset_a, &[game("2025_05_SF_LA", 5)])
            .await
            .unwrap();

        let asset_b = SyncedAsset {
            release_tag: "schedules",
            asset_name: "games.csv",
            updated_at: "2026-07-01T00:00:00Z",
        };
        // Two games sharing a game_id violate the UNIQUE constraint,
        // so the second INSERT fails partway through the batch.
        let bad_batch = [game("2025_06_SF_LA", 6), game("2025_06_SF_LA", 6)];
        let result = replace(&store, &asset_b, &bad_batch).await;
        assert!(result.is_err());

        // The DELETE + partial INSERT must have been rolled back, leaving
        // the pre-failure data untouched.
        let games = for_season(store.reader(), Season(2025)).await.unwrap();
        assert_eq!(games.len(), 1);
        assert_eq!(games[0].game_id, "2025_05_SF_LA");

        // sync_state must not have been updated either.
        assert!(
            sync_state::is_current(
                store.reader(),
                "schedules",
                "games.csv",
                "2026-06-30T10:36:16Z"
            )
            .await
            .unwrap()
        );
        assert!(
            !sync_state::is_current(
                store.reader(),
                "schedules",
                "games.csv",
                "2026-07-01T00:00:00Z"
            )
            .await
            .unwrap()
        );
    }

    #[tokio::test]
    async fn is_current_is_false_when_no_sync_state_row_exists() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&format!("sqlite://{}/t.db", dir.path().display()))
            .await
            .unwrap();

        assert!(
            !sync_state::is_current(store.reader(), "schedules", "games.csv", "anything")
                .await
                .unwrap()
        );
    }
}
