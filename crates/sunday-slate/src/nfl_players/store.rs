use std::collections::HashMap;

use sqlx::Sqlite;

use crate::media::{Media, MediaId, store as media_store};
use crate::nfl_players::model::NflPlayer;

/// Look up a crosswalk row by the external salary-source player id.
pub async fn by_fd_id<'e, E>(ex: E, fd_player_id: &str) -> Result<Option<NflPlayer>, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    sqlx::query_as!(
        NflPlayer,
        r#"SELECT id AS "id!: i64",
                  gsis_player_id AS "gsis_player_id!: String",
                  fd_player_id AS "fd_player_id: String",
                  headshot_media_id AS "headshot_media_id?: MediaId"
           FROM nfl_players WHERE fd_player_id = ?"#,
        fd_player_id,
    )
    .fetch_optional(ex)
    .await
}

/// Lazily create the crosswalk row for a matched player. Idempotent on
/// gsis_player_id: a second call with the same gsis backfills fd_player_id.
pub async fn upsert<'e, E>(
    ex: E,
    gsis_player_id: &str,
    fd_player_id: &str,
) -> Result<NflPlayer, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    sqlx::query_as!(
        NflPlayer,
        r#"INSERT INTO nfl_players (gsis_player_id, fd_player_id) VALUES (?1, ?2)
           ON CONFLICT (gsis_player_id) DO UPDATE SET fd_player_id = ?2
           RETURNING id AS "id!: i64",
                     gsis_player_id AS "gsis_player_id!: String",
                     fd_player_id AS "fd_player_id: String",
                     headshot_media_id AS "headshot_media_id?: MediaId""#,
        gsis_player_id,
        fd_player_id,
    )
    .fetch_one(ex)
    .await
}

pub async fn missing_headshots<'e, E>(ex: E) -> Result<Vec<NflPlayer>, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    sqlx::query_as!(
        NflPlayer,
        r#"SELECT id AS "id!: i64",
                  gsis_player_id AS "gsis_player_id!: String",
                  fd_player_id AS "fd_player_id: String",
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

/// Count of crosswalk players with no cached headshot.
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
    async fn upsert_then_lookup(pool: SqlitePool) {
        assert!(by_fd_id(&pool, "166092").await.expect("miss").is_none());
        let p = upsert(&pool, "00-0036355", "166092").await.expect("upsert");
        assert_eq!(p.gsis_player_id, "00-0036355");
        let found = by_fd_id(&pool, "166092").await.expect("hit").expect("some");
        assert_eq!(found.id, p.id);
    }

    #[sqlx::test]
    async fn upsert_backfills_fd_id_on_gsis_conflict(pool: SqlitePool) {
        let first = upsert(&pool, "00-0036355", "111").await.expect("first");
        let second = upsert(&pool, "00-0036355", "222").await.expect("second");
        assert_eq!(first.id, second.id);
        assert_eq!(second.fd_player_id.as_deref(), Some("222"));
    }
    #[sqlx::test]
    async fn headshot_missing_and_batch_queries_follow_media_links(pool: SqlitePool) {
        let linked = upsert(&pool, "00-LINKED", "linked").await.expect("linked");
        let missing = upsert(&pool, "00-MISSING", "missing")
            .await
            .expect("missing");
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
            upsert(&pool, "00-LINKED", "linked-again")
                .await
                .expect("preserve link")
                .headshot_media_id,
            Some(media_id)
        );
        assert_eq!(missing.id, missing_rows[0].id);
    }
}
