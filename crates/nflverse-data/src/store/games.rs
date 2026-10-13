use crate::error::NflDataError;
use crate::model::{Game, Season};
use crate::store::Store;
use crate::store::sync_state::{self, SyncedAsset};
use sqlx::{Row, SqliteConnection, SqlitePool};

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
            g.season_week.season,
            g.season_week.week,
            g.season_week.season_type,
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
                    g.season_week.season,
                    g.season_week.week,
                    g.season_week.season_type,
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
               gsis_game_id,
               season,
               week,
               -- Normalize legacy labels while retaining numeric SQLx enum encodings.
               CAST(CASE season_type
                   WHEN 'REG' THEN 0
                   WHEN 'POST' THEN 1
                   ELSE season_type
               END AS INTEGER) AS season_type,
               kickoff,
               home_team,
               away_team,
               home_score,
               away_score
           FROM games
           WHERE season = ?
           ORDER BY week, gsis_game_id"#,
    )
    .bind(season)
    .try_map(|row| {
        Ok(Game {
            gsis_game_id: row.try_get("gsis_game_id")?,
            season_week: crate::model::SeasonWeek {
                season: row.try_get("season")?,
                week: row.try_get("week")?,
                season_type: row.try_get("season_type")?,
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
    use crate::model::{SeasonType, TeamAbbr, Week};
    use time::macros::datetime;

    use super::*;

    fn game(id: &str, week: u8) -> Game {
        Game {
            gsis_game_id: id.to_string(),
            season_week: crate::model::SeasonWeek {
                season: Season(2025),
                week: Week(week),
                season_type: SeasonType::Reg,
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
        assert_eq!(games[0].gsis_game_id, "2025_06_SF_LA");
        assert_eq!(games[0].kickoff, Some(datetime!(2025-10-03 00:15 UTC)));
    }

    #[tokio::test]
    async fn for_season_decodes_text_and_integer_season_types() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&format!("sqlite://{}/t.db", dir.path().display()))
            .await
            .unwrap();

        store
            .write_tx(async |conn| {
                for (id, week, season_type) in [
                    ("2025_20_REG", 20, "REG"),
                    ("2025_21_POST", 21, "POST"),
                ] {
                    sqlx::query(
                        "INSERT INTO games (gsis_game_id, season, week, season_type, home_team, away_team) VALUES (?, 2025, ?, ?, 'LA', 'SF')",
                    )
                    .bind(id)
                    .bind(week)
                    .bind(season_type)
                    .execute(&mut *conn)
                    .await?;
                }
                let integer_encoded = [
                    game("2025_19_INT_REG", 19),
                    Game {
                        season_week: crate::model::SeasonWeek {
                            season: Season(2025),
                            week: Week(18),
                            season_type: SeasonType::Post,
                        },
                        ..game("2025_18_INT_POST", 18)
                    },
                ];
                seed(conn, &integer_encoded).await?;
                Ok(())
            })
            .await
            .unwrap();

        let games = for_season(store.reader(), Season(2025)).await.unwrap();
        let season_types: std::collections::HashMap<_, _> = games
            .into_iter()
            .map(|game| (game.gsis_game_id, game.season_week.season_type))
            .collect();
        assert_eq!(season_types["2025_20_REG"], SeasonType::Reg);
        assert_eq!(season_types["2025_21_POST"], SeasonType::Post);
        assert_eq!(season_types["2025_19_INT_REG"], SeasonType::Reg);
        assert_eq!(season_types["2025_18_INT_POST"], SeasonType::Post);
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
