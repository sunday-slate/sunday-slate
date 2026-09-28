use std::collections::HashMap;

use crate::media::{Media, MediaId};
use sqlx::{AssertSqlSafe, Sqlite};
use time::PrimitiveDateTime;

pub async fn insert<'e, E>(
    ex: E,
    path: &str,
    content_type: &str,
    byte_size: i64,
    width: i64,
    height: i64,
) -> Result<MediaId, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    sqlx::query_scalar!(
        r#"INSERT INTO media (path, content_type, byte_size, width, height)
           VALUES (?1, ?2, ?3, ?4, ?5)
           RETURNING id AS "id!: MediaId""#,
        path,
        content_type,
        byte_size,
        width,
        height,
    )
    .fetch_one(ex)
    .await
}

pub async fn update_bytes<'e, E>(
    ex: E,
    id: MediaId,
    byte_size: i64,
    width: i64,
    height: i64,
) -> Result<(), sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    sqlx::query!(
        "UPDATE media SET byte_size = ?2, width = ?3, height = ?4 WHERE id = ?1",
        id,
        byte_size,
        width,
        height,
    )
    .execute(ex)
    .await
    .map(|_| ())
}

pub async fn get<'e, E>(ex: E, id: MediaId) -> Result<Option<Media>, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    sqlx::query_as!(
        Media,
        r#"SELECT id AS "id!: MediaId",
                  path AS "path!: String",
                  content_type AS "content_type!: String",
                  byte_size AS "byte_size!: i64",
                  width AS "width!: i64",
                  height AS "height!: i64",
                  created_at AS "created_at!: PrimitiveDateTime",
                  updated_at AS "updated_at!: PrimitiveDateTime"
           FROM media WHERE id = ?1"#,
        id,
    )
    .fetch_optional(ex)
    .await
}

/// The row at `path`, if one exists. `media.path` is unique.
pub async fn get_by_path<'e, E>(ex: E, path: &str) -> Result<Option<Media>, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    sqlx::query_as!(
        Media,
        r#"SELECT id AS "id!: MediaId",
                  path AS "path!: String",
                  content_type AS "content_type!: String",
                  byte_size AS "byte_size!: i64",
                  width AS "width!: i64",
                  height AS "height!: i64",
                  created_at AS "created_at!: PrimitiveDateTime",
                  updated_at AS "updated_at!: PrimitiveDateTime"
           FROM media WHERE path = ?1"#,
        path,
    )
    .fetch_optional(ex)
    .await
}

/// One media row keyed by its owning entity, for batch lookups.
#[derive(Debug, sqlx::FromRow)]
struct KeyedMedia {
    key: String,
    #[sqlx(flatten)]
    media: Media,
}

/// Run `sql`, which must select `key` plus every `media` column (`m.*`),
/// binding `keys` in order. Returns the rows keyed by `key`.
pub async fn fetch_keyed<'e, E>(
    ex: E,
    sql: &str,
    keys: &[&str],
) -> Result<HashMap<String, Media>, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    let sql = AssertSqlSafe(sql);
    let mut query = sqlx::query_as::<_, KeyedMedia>(sql);
    for key in keys {
        query = query.bind(key);
    }
    let rows = query.fetch_all(ex).await?;
    Ok(rows.into_iter().map(|row| (row.key, row.media)).collect())
}

/// The `media` columns a caller `LEFT JOIN`s onto an owning row.
pub(crate) struct LogoColumns {
    pub id: Option<MediaId>,
    pub path: Option<String>,
    pub content_type: Option<String>,
    pub byte_size: Option<i64>,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub created_at: Option<PrimitiveDateTime>,
    pub updated_at: Option<PrimitiveDateTime>,
}

/// `path` gates the join: a media row without one isn't servable yet. The
/// expects encode the remaining columns' NOT NULL.
pub(crate) fn build_logo(cols: LogoColumns) -> Option<Media> {
    cols.id.zip(cols.path).map(|(id, path)| Media {
        id,
        path,
        content_type: cols.content_type.expect("media.content_type NOT NULL"),
        byte_size: cols.byte_size.expect("media.byte_size NOT NULL"),
        width: cols.width.expect("media.width NOT NULL"),
        height: cols.height.expect("media.height NOT NULL"),
        created_at: cols.created_at.expect("media.created_at NOT NULL"),
        updated_at: cols.updated_at.expect("media.updated_at NOT NULL"),
    })
}

pub async fn delete<'e, E>(ex: E, id: MediaId) -> Result<(), sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    sqlx::query!("DELETE FROM media WHERE id = ?1", id)
        .execute(ex)
        .await
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::SqlitePool;

    #[sqlx::test]
    async fn insert_get_roundtrip(pool: SqlitePool) {
        let mut conn = pool.acquire().await.expect("conn");
        let id = insert(&mut *conn, "7.png", "image/png", 1234, 256, 256)
            .await
            .expect("insert");
        let m = get(&pool, id).await.expect("get").expect("some");
        assert_eq!(m.id, id);
        assert_eq!(m.path, "7.png");
        assert_eq!(m.content_type, "image/png");
        assert_eq!(m.width, 256);
    }

    #[sqlx::test]
    async fn delete_removes_row(pool: SqlitePool) {
        let mut conn = pool.acquire().await.expect("conn");
        let id = insert(&mut *conn, "8.png", "image/png", 1, 256, 256)
            .await
            .expect("insert");
        delete(&mut *conn, id).await.expect("delete");
        assert!(get(&pool, id).await.expect("get").is_none());
    }
}
