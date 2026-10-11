use std::collections::HashMap;

use sqlx::Sqlite;

use crate::media::{Media, MediaId, store as media_store};
use crate::nfl_players::model::NflPlayer;

/// Ensure a canonical player has a local row for headshot bookkeeping.
pub async fn upsert_for_gsis<'e, E>(ex: E, gsis_player_id: &str) -> Result<NflPlayer, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    sqlx::query_as::<_, NflPlayer>(
        r#"INSERT INTO nfl_players (gsis_player_id) VALUES (?1)
           ON CONFLICT (gsis_player_id) DO UPDATE SET gsis_player_id = excluded.gsis_player_id
           RETURNING id, gsis_player_id, headshot_media_id"#,
    )
    .bind(gsis_player_id)
    .fetch_one(ex)
    .await
}

pub async fn ensure_players_for_gsis(
    conn: &mut sqlx::SqliteConnection,
    gsis_player_ids: &[String],
) -> Result<(), sqlx::Error> {
    for gsis_player_id in gsis_player_ids {
        sqlx::query("INSERT OR IGNORE INTO nfl_players (gsis_player_id) VALUES (?1)")
            .bind(gsis_player_id)
            .execute(&mut *conn)
            .await?;
    }
    Ok(())
}

pub async fn by_gsis_id<'e, E>(
    ex: E,
    gsis_player_id: &str,
) -> Result<Option<NflPlayer>, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    sqlx::query_as::<_, NflPlayer>(
        r#"SELECT id, gsis_player_id, headshot_media_id
           FROM nfl_players WHERE gsis_player_id = ?1"#,
    )
    .bind(gsis_player_id)
    .fetch_optional(ex)
    .await
}

pub async fn missing_headshots<'e, E>(ex: E) -> Result<Vec<NflPlayer>, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    sqlx::query_as::<_, NflPlayer>(
        r#"SELECT id, gsis_player_id, headshot_media_id
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

pub async fn count_missing_headshots<'e, E>(ex: E) -> Result<i64, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    sqlx::query_scalar!(
        r#"SELECT COUNT(*) AS "count!: i64" FROM nfl_players WHERE headshot_media_id IS NULL"#
    )
    .fetch_one(ex)
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
    async fn canonical_player_upsert_preserves_media_association(pool: SqlitePool) {
        let player = upsert_for_gsis(&pool, "00-LINKED").await.expect("player");
        let media_id = media_store::insert(&pool, "headshot.png", "image/png", 12, 256, 128)
            .await
            .expect("media");
        set_headshot_media_id(&pool, player.id, media_id)
            .await
            .expect("link");
        let again = upsert_for_gsis(&pool, "00-LINKED").await.expect("again");
        assert_eq!(again.id, player.id);
        assert_eq!(again.headshot_media_id, Some(media_id));

        let missing = upsert_for_gsis(&pool, "00-MISSING").await.expect("missing");
        let missing_rows = missing_headshots(&pool).await.expect("missing rows");
        assert_eq!(missing_rows.len(), 1);
        assert_eq!(missing_rows[0].gsis_player_id, "00-MISSING");
        let rows = headshots_for(&pool, &["00-LINKED", "00-MISSING", "00-UNKNOWN"])
            .await
            .expect("headshots");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows["00-LINKED"].id, media_id);
        assert!(headshots_for(&pool, &[]).await.expect("empty").is_empty());
        assert_eq!(missing.id, missing_rows[0].id);
    }
}
