use sqlx::Sqlite;

/// Row type for password_reset_tokens table queries.
#[derive(sqlx::FromRow)]
pub(crate) struct ActiveToken {
    pub(crate) id: i64,
    pub(crate) user_id: i64,
    pub(crate) token_hash: String,
}

/// Insert a new reset token (or replace an existing one for the same user).
/// Returns the token's row ID.
pub async fn insert_token<'e, E>(ex: E, user_id: i64, token_hash: &str) -> Result<i64, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    sqlx::query_scalar!(
        "INSERT INTO password_reset_tokens (user_id, token_hash, expires_at) \
         VALUES (?1, ?2, datetime('now','subsec','+1 hour')) \
         ON CONFLICT(user_id) DO UPDATE SET \
             token_hash = excluded.token_hash, \
             expires_at = excluded.expires_at, \
             used_at    = NULL, \
             created_at = datetime('now','subsec') \
         RETURNING id",
        user_id,
        token_hash,
    )
    .fetch_one(ex)
    .await
}

/// Return an active (unused, unexpired) token, if one exists for the given ID.
pub async fn find_active_token<'e, E>(ex: E, id: i64) -> Result<Option<ActiveToken>, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    sqlx::query_as!(
        ActiveToken,
        r#"SELECT id AS "id!: i64",
                  user_id AS "user_id!: i64",
                  token_hash AS "token_hash!: String"
           FROM password_reset_tokens
           WHERE id = ?1 AND used_at IS NULL AND expires_at > datetime('now','subsec')"#,
        id,
    )
    .fetch_optional(ex)
    .await
}

/// Mark a token as used, invalidating it.
pub async fn mark_token_used<'e, E>(ex: E, id: i64) -> Result<(), sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    sqlx::query!(
        "UPDATE password_reset_tokens SET used_at = datetime('now','subsec') WHERE id = ?1",
        id,
    )
    .execute(ex)
    .await?;
    Ok(())
}

#[cfg(test)] //only used in tests for now
pub async fn token_count<'e, E>(ex: E) -> Result<i64, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    sqlx::query_scalar("SELECT COUNT(*) FROM password_reset_tokens")
        .fetch_one(ex)
        .await
}
