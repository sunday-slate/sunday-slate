use sqlx::{SqliteConnection, SqlitePool};
use time::OffsetDateTime;

use crate::error::NflDataError;
use crate::model::{Game, Season, SeasonType, TeamAbbr, Week};
use crate::store::Store;
use crate::store::sync_state::{self, SyncedAsset};

/// Insert games directly, bypassing the sync pipeline and `sync_state`
/// bookkeeping. Runs inside a caller-supplied transaction. For tests and
/// tooling only — see `NflData::seed_for_test`.
pub(crate) async fn seed(conn: &mut SqliteConnection, games: &[Game]) -> Result<(), NflDataError> {
    for g in games {
        sqlx::query!(
            r#"INSERT INTO games
               (gsis_game_id, season, week, season_type, kickoff,
                home_team, away_team, home_score, away_score)
               VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
            g.gsis_game_id,
            g.season,
            g.week,
            g.season_type,
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
                sqlx::query!(
                    r#"INSERT INTO games
                       (gsis_game_id, season, week, season_type, kickoff,
                        home_team, away_team, home_score, away_score)
                       VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
                    g.gsis_game_id,
                    g.season,
                    g.week,
                    g.season_type,
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
    Ok(sqlx::query_as!(
        Game,
        r#"SELECT
               gsis_game_id,
               season AS "season: Season",
               week AS "week: Week",
               season_type AS "season_type: SeasonType",
               kickoff AS "kickoff: OffsetDateTime",
               home_team AS "home_team: TeamAbbr",
               away_team AS "away_team: TeamAbbr",
               home_score AS "home_score: i32",
               away_score AS "away_score: i32"
           FROM games
           WHERE season = ?
           ORDER BY week, gsis_game_id"#,
        season
    )
    .fetch_all(pool)
    .await?)
}

#[cfg(test)]
mod tests {
    use time::macros::datetime;

    use super::*;

    fn game(id: &str, week: u8) -> Game {
        Game {
            gsis_game_id: id.to_string(),
            season: Season(2025),
            week: Week(week),
            season_type: SeasonType::Reg,
            kickoff: Some(datetime!(2025-10-03 00:15 UTC)),
            home_team: TeamAbbr("LA".into()),
            away_team: TeamAbbr("SF".into()),
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
        assert_eq!(games[0].gsis_game_id, "2025_06_SF_LA");
        assert_eq!(games[0].kickoff, Some(datetime!(2025-10-03 00:15 UTC)));
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
        // Two games sharing a gsis_game_id violate the UNIQUE constraint,
        // so the second INSERT fails partway through the batch.
        let bad_batch = [game("2025_06_SF_LA", 6), game("2025_06_SF_LA", 6)];
        let result = replace(&store, &asset_b, &bad_batch).await;
        assert!(result.is_err());

        // The DELETE + partial INSERT must have been rolled back, leaving
        // the pre-failure data untouched.
        let games = for_season(store.reader(), Season(2025)).await.unwrap();
        assert_eq!(games.len(), 1);
        assert_eq!(games[0].gsis_game_id, "2025_05_SF_LA");

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
