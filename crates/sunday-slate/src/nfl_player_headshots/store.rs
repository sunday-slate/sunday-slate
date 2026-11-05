use std::collections::HashMap;

use nfl_model::NflPlayerId;
use sqlx::Sqlite;

use crate::media::{Media, MediaId, store as media_store};

/// Cached gsis ids. Table row is the marker, even if the file is gone.
pub async fn cached_player_ids<'e, E>(ex: E) -> Result<Vec<NflPlayerId>, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    sqlx::query_scalar::<_, NflPlayerId>("SELECT nfl_player_id FROM nfl_player_headshots")
        .fetch_all(ex)
        .await
}

pub async fn set_headshot_media_id<'e, E>(
    ex: E,
    nfl_player_id: &NflPlayerId,
    media_id: MediaId,
) -> Result<(), sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    sqlx::query!(
        r#"INSERT INTO nfl_player_headshots (nfl_player_id, headshot_media_id) VALUES (?1, ?2)
           ON CONFLICT (nfl_player_id) DO UPDATE SET headshot_media_id = excluded.headshot_media_id"#,
        nfl_player_id,
        media_id,
    )
    .execute(ex)
    .await
    .map(|_| ())
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
        r#"SELECT h.nfl_player_id AS key, m.*
           FROM nfl_player_headshots h
           JOIN media m ON m.id = h.headshot_media_id
           WHERE h.nfl_player_id IN ({placeholders})"#
    );
    media_store::fetch_keyed(ex, &sql, gsis_ids).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::store as media_store;
    use sqlx::SqlitePool;

    #[sqlx::test]
    async fn headshot_batch_lookup_follows_media_links(pool: SqlitePool) {
        let media_id = media_store::insert(&pool, "headshot.png", "image/png", 12, 256, 128)
            .await
            .expect("media");
        set_headshot_media_id(&pool, &NflPlayerId("00-LINKED".into()), media_id)
            .await
            .expect("link");

        let rows = headshots_for(&pool, &["00-LINKED", "00-MISSING", "00-UNKNOWN"])
            .await
            .expect("headshots");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows["00-LINKED"].id, media_id);
        assert!(headshots_for(&pool, &[]).await.expect("empty").is_empty());
    }
}
