use sqlx::Sqlite;

use crate::contests::ContestId;
use crate::contests::model::Contest;

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
