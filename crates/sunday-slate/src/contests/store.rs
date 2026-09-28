use std::collections::HashMap;

use itertools::Itertools;
use sqlx::{Sqlite, SqliteConnection};

use crate::contests::model::Contest;
use crate::contests::{ContestId, NflGameId};

pub async fn all<'e, E>(ex: E) -> Result<Vec<Contest>, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    sqlx::query_as!(
        Contest,
        r#"SELECT id AS "id!: ContestId", name AS "name!: String" FROM contests ORDER BY id"#
    )
    .fetch_all(ex)
    .await
}

pub async fn by_id<'e, E>(ex: E, id: ContestId) -> Result<Option<Contest>, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    sqlx::query_as!(
        Contest,
        r#"SELECT id AS "id!: ContestId", name AS "name!: String" FROM contests WHERE id = ?1"#,
        id
    )
    .fetch_optional(ex)
    .await
}

/// Create a contest by name if absent. Idempotent via `INSERT OR IGNORE`
/// (`name` is UNIQUE). The setup workflow composes this; general callers may
/// also add a contest programmatically.
pub async fn create(conn: &mut SqliteConnection, name: &str) -> Result<(), sqlx::Error> {
    sqlx::query!("INSERT OR IGNORE INTO contests (name) VALUES (?1)", name)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

pub async fn game_ids<'e, E>(ex: E, contest_id: ContestId) -> Result<Vec<String>, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    sqlx::query_scalar!(
        r#"SELECT gsis_game_id AS "gsis_game_id!: String"
           FROM contest_games WHERE contest_id = ?1 ORDER BY gsis_game_id"#,
        contest_id
    )
    .fetch_all(ex)
    .await
}

/// Replace a contest's published slate with exactly `gsis_game_ids`. Deletes
/// the rows absent from `gsis_game_ids`, inserts the rows missing, and leaves
/// the rest alone, so a repeat call with the same ids changes nothing.
pub async fn set_games(
    conn: &mut SqliteConnection,
    contest_id: ContestId,
    gsis_game_ids: &[String],
) -> Result<(), sqlx::Error> {
    let current = game_ids(&mut *conn, contest_id).await?;
    for gid in current.iter().filter(|g| !gsis_game_ids.contains(g)) {
        sqlx::query!(
            "DELETE FROM contest_games WHERE contest_id = ?1 AND gsis_game_id = ?2",
            contest_id,
            gid
        )
        .execute(&mut *conn)
        .await?;
    }
    for gid in gsis_game_ids {
        sqlx::query!(
            "INSERT OR IGNORE INTO contest_games (contest_id, gsis_game_id) VALUES (?1, ?2)",
            contest_id,
            gid
        )
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}

/// Every materialized contest's game ids, keyed by contest. One query for the
/// whole season — the standings page resolves each contest's NFL week from
/// this.
pub async fn materialized_games<'e, E>(
    ex: E,
) -> Result<HashMap<ContestId, Vec<NflGameId>>, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    let rows = sqlx::query!(
        r#"SELECT contest_id AS "contest_id!: ContestId", gsis_game_id AS "gsis_game_id!: NflGameId"
           FROM contest_games ORDER BY contest_id, gsis_game_id"#
    )
    .fetch_all(ex)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| (r.contest_id, r.gsis_game_id))
        .into_group_map())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Db;
    use sqlx::SqlitePool;

    #[sqlx::test]
    async fn materialized_games_maps_each_contest_to_its_games(pool: SqlitePool) {
        let db = Db::test(pool.clone());
        db.write_tx::<_, (), sqlx::Error>(async |conn| {
            create(conn, "Week 1").await?;
            create(conn, "Week 2").await?;
            let c1 = by_id(&mut *conn, ContestId(1)).await?.unwrap();
            let c2 = by_id(&mut *conn, ContestId(2)).await?.unwrap();
            set_games(
                &mut *conn,
                c1.id,
                &["2025_01_A_B".to_string(), "2025_01_C_D".to_string()],
            )
            .await?;
            set_games(&mut *conn, c2.id, &["2025_02_A_B".to_string()]).await
        })
        .await
        .unwrap();

        let map = materialized_games(&pool).await.unwrap();
        assert_eq!(map.len(), 2);
        assert_eq!(
            map[&ContestId(1)],
            vec![
                NflGameId("2025_01_A_B".into()),
                NflGameId("2025_01_C_D".into())
            ]
        );
        assert_eq!(map[&ContestId(2)], vec![NflGameId("2025_02_A_B".into())]);
    }

    #[sqlx::test]
    async fn set_games_adds_removes_and_is_idempotent(pool: SqlitePool) {
        let db = Db::test(pool.clone());
        db.write_tx::<_, (), sqlx::Error>(async |conn| {
            create(conn, "Week 1").await?;
            let c = by_id(&mut *conn, ContestId(1)).await?.unwrap();
            set_games(conn, c.id, &["A".to_string(), "B".to_string()]).await
        })
        .await
        .unwrap();
        assert_eq!(
            game_ids(&pool, ContestId(1)).await.unwrap(),
            vec!["A".to_string(), "B".to_string()]
        );

        // B is unchecked, C is checked, A is left alone.
        db.write_tx::<_, (), sqlx::Error>(async |conn| {
            set_games(conn, ContestId(1), &["A".to_string(), "C".to_string()]).await
        })
        .await
        .unwrap();
        assert_eq!(
            game_ids(&pool, ContestId(1)).await.unwrap(),
            vec!["A".to_string(), "C".to_string()]
        );

        db.write_tx::<_, (), sqlx::Error>(async |conn| {
            set_games(conn, ContestId(1), &["A".to_string(), "C".to_string()]).await
        })
        .await
        .unwrap();
        assert_eq!(
            game_ids(&pool, ContestId(1)).await.unwrap(),
            vec!["A".to_string(), "C".to_string()]
        );
    }

    #[sqlx::test]
    async fn create_is_idempotent(pool: SqlitePool) {
        let db = Db::test(pool.clone());
        db.write_tx::<_, (), sqlx::Error>(async |conn| {
            create(conn, "Week 1").await?;
            create(conn, "Week 1").await // second create is a no-op (name is UNIQUE)
        })
        .await
        .unwrap();
        assert_eq!(all(&pool).await.unwrap().len(), 1);
    }
}
