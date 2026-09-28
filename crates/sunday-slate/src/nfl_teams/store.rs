use std::collections::HashMap;

use sqlx::Sqlite;

use crate::media::{Media, MediaId, store as media_store};
use crate::nfl_teams::model::NflTeam;

pub async fn missing_logos<'e, E>(ex: E) -> Result<Vec<NflTeam>, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    sqlx::query_as!(
        NflTeam,
        r#"SELECT abbr AS "abbr!: String",
                  logo_media_id AS "logo_media_id?: MediaId"
           FROM nfl_teams
           WHERE logo_media_id IS NULL
           ORDER BY abbr"#,
    )
    .fetch_all(ex)
    .await
}

pub async fn set_logo_media_id<'e, E>(
    ex: E,
    abbr: &str,
    media_id: MediaId,
) -> Result<(), sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    sqlx::query!(
        "UPDATE nfl_teams SET logo_media_id = ?2 WHERE abbr = ?1",
        abbr,
        media_id,
    )
    .execute(ex)
    .await
    .map(|_| ())
}

/// Count of teams with no cached logo.
pub async fn count_missing_logos<'e, E>(ex: E) -> Result<i64, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    sqlx::query_scalar!(
        r#"SELECT COUNT(*) AS "count!: i64" FROM nfl_teams WHERE logo_media_id IS NULL"#
    )
    .fetch_one(ex)
    .await
}

pub async fn logos_for<'e, E>(ex: E, abbrs: &[&str]) -> Result<HashMap<String, Media>, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    if abbrs.is_empty() {
        return Ok(HashMap::new());
    }
    let placeholders = std::iter::repeat_n("?", abbrs.len())
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        r#"SELECT t.abbr AS key, m.*
           FROM nfl_teams t
           JOIN media m ON m.id = t.logo_media_id
           WHERE t.abbr IN ({placeholders})"#
    );
    media_store::fetch_keyed(ex, &sql, abbrs).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::store as media_store;
    use sqlx::SqlitePool;

    #[sqlx::test]
    async fn seeded_teams_and_logo_queries_follow_media_links(pool: SqlitePool) {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM nfl_teams")
            .fetch_one(&pool)
            .await
            .expect("team count");
        assert_eq!(count, 32);
        assert_eq!(missing_logos(&pool).await.expect("missing").len(), 32);

        let media_id = media_store::insert(&pool, "kc-logo.png", "image/png", 8, 128, 128)
            .await
            .expect("media");
        set_logo_media_id(&pool, "KC", media_id)
            .await
            .expect("link logo");
        let missing = missing_logos(&pool).await.expect("missing");
        assert_eq!(missing.len(), 31);
        assert!(!missing.iter().any(|team| team.abbr == "KC"));

        let logos = logos_for(&pool, &["KC", "BUF", "UNKNOWN"])
            .await
            .expect("logos");
        assert_eq!(logos.len(), 1);
        assert_eq!(logos["KC"].id, media_id);
        assert!(logos_for(&pool, &[]).await.expect("empty").is_empty());
    }
}
