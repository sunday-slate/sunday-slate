use sqlx::Sqlite;

use crate::player_salaries::model::NflPlayerSalary;

/// Upsert one per-game salary. Player rows conflict on (game, player);
/// DST rows conflict on (game, team). Matches the two partial unique indexes.
pub async fn upsert(
    conn: &mut sqlx::SqliteConnection,
    s: &NflPlayerSalary,
) -> Result<(), sqlx::Error> {
    if s.dfs_position.is_defense() {
        sqlx::query!(
            r#"INSERT INTO nfl_player_salaries
                 (gsis_game_id, gsis_player_id, team_abbr, dfs_position, salary)
               VALUES (?1, NULL, ?2, 'DST', ?3)
               ON CONFLICT (gsis_game_id, team_abbr) WHERE dfs_position = 'DST'
               DO UPDATE SET salary = ?3"#,
            s.gsis_game_id,
            s.team_abbr,
            s.salary,
        )
        .execute(&mut *conn)
        .await?;
    } else {
        let pos = s.dfs_position;
        sqlx::query!(
            r#"INSERT INTO nfl_player_salaries
                 (gsis_game_id, gsis_player_id, team_abbr, dfs_position, salary)
               VALUES (?1, ?2, ?3, ?4, ?5)
               ON CONFLICT (gsis_game_id, gsis_player_id) WHERE dfs_position <> 'DST'
               DO UPDATE SET salary = ?5, team_abbr = ?3, dfs_position = ?4"#,
            s.gsis_game_id,
            s.gsis_player_id,
            s.team_abbr,
            pos,
            s.salary,
        )
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}

/// Whether any salary row exists for the given games.
pub async fn any_for_games<'e, E>(ex: E, gsis_game_ids: &[&str]) -> Result<bool, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM nfl_player_salaries
         WHERE gsis_game_id IN (SELECT value FROM json_each(?1)))",
    )
    .bind(game_id_json(gsis_game_ids))
    .fetch_one(ex)
    .await
}

/// Every salary row whose game is in the given set — the slate's salary pool.
pub async fn for_games<'e, E>(
    ex: E,
    gsis_game_ids: &[&str],
) -> Result<Vec<NflPlayerSalary>, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    sqlx::query_as::<_, NflPlayerSalary>(
        "SELECT id, gsis_game_id, gsis_player_id, team_abbr, dfs_position, salary
         FROM nfl_player_salaries
         WHERE gsis_game_id IN (SELECT value FROM json_each(?1))
         ORDER BY gsis_game_id, salary DESC",
    )
    .bind(game_id_json(gsis_game_ids))
    .fetch_all(ex)
    .await
}

/// The game ids as a JSON array for `json_each`: one bind parameter and
/// static SQL, whatever the slate's size.
fn game_id_json(gsis_game_ids: &[&str]) -> String {
    serde_json::to_string(gsis_game_ids).expect("string ids serialize")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::player_salaries::model::DfsPosition;
    use sqlx::SqlitePool;

    fn player_salary(game: &str, gsis: &str, salary: i64) -> NflPlayerSalary {
        NflPlayerSalary {
            id: 0,
            gsis_game_id: game.into(),
            gsis_player_id: Some(gsis.into()),
            team_abbr: "SF".into(),
            dfs_position: DfsPosition::Rb,
            salary,
        }
    }

    #[sqlx::test]
    async fn upsert_is_idempotent_and_updates_salary(pool: SqlitePool) {
        let db = crate::Db::test(pool.clone());
        db.write_tx::<_, (), sqlx::Error>(async |conn| {
            upsert(conn, &player_salary("2025_05_SF_LA", "00-1", 8000)).await?;
            upsert(conn, &player_salary("2025_05_SF_LA", "00-1", 8500)).await
        })
        .await
        .expect("upsert");

        let rows = for_games(&pool, &["2025_05_SF_LA"]).await.expect("read");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].salary, 8500);
    }

    #[sqlx::test]
    async fn dst_and_player_share_a_game_without_conflict(pool: SqlitePool) {
        let db = crate::Db::test(pool.clone());
        let dst = NflPlayerSalary {
            id: 0,
            gsis_game_id: "2025_05_SF_LA".into(),
            gsis_player_id: None,
            team_abbr: "SF".into(),
            dfs_position: DfsPosition::Dst,
            salary: 4000,
        };
        db.write_tx::<_, (), sqlx::Error>(async |conn| {
            upsert(conn, &player_salary("2025_05_SF_LA", "00-1", 8000)).await?;
            upsert(conn, &dst).await
        })
        .await
        .expect("upsert");
        let rows = for_games(&pool, &["2025_05_SF_LA"]).await.expect("read");
        assert_eq!(rows.len(), 2);
    }
}
