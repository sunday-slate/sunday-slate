use std::collections::{HashMap, HashSet};

use sqlx::Sqlite;

use crate::media::{Media, MediaId, store as media_store};
use crate::nfl_players::model::NflPlayer;

/// Get-or-create the local player row for one GSIS identity. Media-only:
/// reconciliation links live in nfl-data, not here. Idempotent on
/// gsis_player_id; an existing row keeps its timestamps and headshot.
pub async fn upsert_for_gsis<'e, E>(ex: E, gsis_player_id: &str) -> Result<NflPlayer, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Sqlite> + Copy,
{
    sqlx::query("INSERT INTO nfl_players (gsis_player_id) VALUES (?1) ON CONFLICT (gsis_player_id) DO NOTHING")
        .bind(gsis_player_id)
        .execute(ex)
        .await?;
    sqlx::query_as!(
        NflPlayer,
        r#"SELECT id AS "id!: i64",
                  gsis_player_id AS "gsis_player_id!: String",
                  headshot_media_id AS "headshot_media_id?: MediaId"
           FROM nfl_players WHERE gsis_player_id = ?1"#,
        gsis_player_id,
    )
    .fetch_one(ex)
    .await
}

/// Same as `upsert_for_gsis` for a borrowed connection (no `Copy` needed).
pub async fn upsert_for_gsis_conn(
    conn: &mut sqlx::SqliteConnection,
    gsis_player_id: &str,
) -> Result<NflPlayer, sqlx::Error> {
    let player_id: i64 = match sqlx::query_scalar::<_, i64>(
        "INSERT INTO nfl_players (gsis_player_id) VALUES (?1) ON CONFLICT (gsis_player_id) DO NOTHING RETURNING id",
    )
    .bind(gsis_player_id)
    .fetch_optional(&mut *conn)
    .await?
    {
        Some(id) => id,
        None => {
            sqlx::query_scalar::<_, i64>("SELECT id FROM nfl_players WHERE gsis_player_id = ?1")
                .bind(gsis_player_id)
                .fetch_one(&mut *conn)
                .await?
        }
    };

    sqlx::query_as!(
        NflPlayer,
        r#"SELECT id AS "id!: i64",
                  gsis_player_id AS "gsis_player_id!: String",
                  headshot_media_id AS "headshot_media_id?: MediaId"
           FROM nfl_players WHERE id = ?1"#,
        player_id,
    )
    .fetch_one(&mut *conn)
    .await
}
pub async fn missing_headshots<'e, E>(ex: E) -> Result<Vec<NflPlayer>, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Sqlite> + Copy,
{
    sqlx::query_as!(
        NflPlayer,
        r#"SELECT id AS "id!: i64",
                  gsis_player_id AS "gsis_player_id!: String",
                  headshot_media_id AS "headshot_media_id?: MediaId"
           FROM nfl_players
           WHERE headshot_media_id IS NULL
           ORDER BY id"#,
    )
    .fetch_all(ex)
    .await
}

pub async fn set_headshot_media_id<'e, E>(
    ex: E,
    player_id: i64,
    media_id: MediaId,
) -> Result<(), sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    sqlx::query!(
        "UPDATE nfl_players SET headshot_media_id = ?2 WHERE id = ?1",
        player_id,
        media_id,
    )
    .execute(ex)
    .await
    .map(|_| ())
}

/// GSIS ids of local player rows that have no headshot reference.
pub(crate) async fn headshot_player_ids<'e, E>(ex: E) -> Result<HashSet<String>, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    let ids = sqlx::query_scalar!(
        r#"SELECT gsis_player_id AS "gsis_player_id!: String"
           FROM nfl_players WHERE headshot_media_id IS NULL"#,
    )
    .fetch_all(ex)
    .await?;
    Ok(ids.into_iter().collect())
}

/// GSIS ids of local player rows that HAVE a headshot reference.
pub(crate) async fn headshot_ids_with_media<'e, E>(ex: E) -> Result<Vec<String>, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    sqlx::query_scalar!(
        r#"SELECT gsis_player_id AS "gsis_player_id!: String"
           FROM nfl_players WHERE headshot_media_id IS NOT NULL"#,
    )
    .fetch_all(ex)
    .await
}

pub async fn headshots_for<'e, E>(
    ex: E,
    gsis_ids: &[&str],
) -> Result<HashMap<String, Media>, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    if gsis_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let placeholders = std::iter::repeat_n("?", gsis_ids.len())
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        r#"SELECT p.gsis_player_id AS key, m.*
           FROM nfl_players p
           JOIN media m ON m.id = p.headshot_media_id
           WHERE p.gsis_player_id IN ({placeholders})"#
    );
    media_store::fetch_keyed(ex, &sql, gsis_ids).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::store as media_store;
    use sqlx::SqlitePool;

    #[sqlx::test]
    async fn upsert_for_gsis_then_lookup(pool: SqlitePool) {
        let p = upsert_for_gsis(&pool, "00-0036355").await.expect("upsert");
        assert_eq!(p.gsis_player_id, "00-0036355");
        assert_eq!(p.headshot_media_id, None);
        let again = upsert_for_gsis(&pool, "00-0036355")
            .await
            .expect("idempotent");
        assert_eq!(again.id, p.id);
    }

    #[sqlx::test]
    async fn headshot_missing_and_batch_queries_follow_media_links(pool: SqlitePool) {
        let linked = upsert_for_gsis(&pool, "00-LINKED").await.expect("linked");
        let missing = upsert_for_gsis(&pool, "00-MISSING").await.expect("missing");
        let media_id = media_store::insert(&pool, "headshot.png", "image/png", 12, 256, 128)
            .await
            .expect("media");
        set_headshot_media_id(&pool, linked.id, media_id)
            .await
            .expect("link");

        let missing_rows = missing_headshots(&pool).await.expect("missing rows");
        assert_eq!(
            missing_rows
                .iter()
                .map(|row| row.gsis_player_id.as_str())
                .collect::<Vec<_>>(),
            vec!["00-MISSING"]
        );
        let rows = headshots_for(&pool, &["00-LINKED", "00-MISSING", "00-UNKNOWN"])
            .await
            .expect("headshots");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows["00-LINKED"].id, media_id);
        assert!(headshots_for(&pool, &[]).await.expect("empty").is_empty());
        assert_eq!(
            upsert_for_gsis(&pool, "00-LINKED")
                .await
                .expect("preserve link")
                .headshot_media_id,
            Some(media_id)
        );
        assert_eq!(missing.id, missing_rows[0].id);
    }
}
