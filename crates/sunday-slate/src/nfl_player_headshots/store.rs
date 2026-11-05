use std::collections::HashMap;

use sqlx::Sqlite;

use crate::media::{Media, MediaId, store as media_store};

/// Player discovery is intentionally disabled until its replacement lands:
/// nothing enumerates players that still need a headshot, so a fetch run sees
/// no player candidates and only refreshes team logos.
pub async fn missing_headshots<'e, E>(_ex: E) -> Result<Vec<nfl_model::NflPlayerId>, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    Ok(Vec::new())
}

pub async fn set_headshot_media_id<'e, E>(
    ex: E,
    gsis_player_id: &str,
    media_id: MediaId,
) -> Result<(), sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    sqlx::query!(
        r#"INSERT INTO nfl_player_headshots (nfl_player_id, headshot_media_id) VALUES (?1, ?2)
           ON CONFLICT (nfl_player_id) DO UPDATE SET headshot_media_id = excluded.headshot_media_id"#,
        gsis_player_id,
        media_id,
    )
    .execute(ex)
    .await
    .map(|_| ())
}

/// Count of players with no cached headshot. Zero while discovery is disabled.
pub async fn count_missing_headshots<'e, E>(_ex: E) -> Result<i64, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    Ok(0)
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
        set_headshot_media_id(&pool, "00-LINKED", media_id)
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
