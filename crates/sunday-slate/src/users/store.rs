use crate::User;
use crate::users::model::UserListItem;
use sqlx::Sqlite;
use time::PrimitiveDateTime;

pub async fn list_all<'e, E>(ex: E) -> Result<Vec<UserListItem>, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    sqlx::query_as!(
        UserListItem,
        r#"SELECT id AS "id!: i64",
                  email AS "email!: String",
                  created_at AS "created_at!: PrimitiveDateTime"
           FROM users
           ORDER BY created_at DESC"#,
    )
    .fetch_all(ex)
    .await
}

pub async fn find_by_email<'e, E>(ex: E, email: &str) -> Result<Option<User>, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    let lower = email.to_lowercase();
    sqlx::query_as!(
        User,
        r#"SELECT id AS "id!: i64",
                  email AS "email!: String",
                  password_hash AS "password_hash!: String",
                  is_admin AS "is_admin!: bool",
                  created_at AS "created_at!: PrimitiveDateTime",
                  updated_at AS "updated_at!: PrimitiveDateTime"
           FROM users
           WHERE email = ?"#,
        lower,
    )
    .fetch_optional(ex)
    .await
}

pub async fn find_by_id<'e, E>(ex: E, id: i64) -> Result<Option<User>, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    sqlx::query_as!(
        User,
        r#"SELECT id AS "id!: i64",
                  email AS "email!: String",
                  password_hash AS "password_hash!: String",
                  is_admin AS "is_admin!: bool",
                  created_at AS "created_at!: PrimitiveDateTime",
                  updated_at AS "updated_at!: PrimitiveDateTime"
           FROM users
           WHERE id = ?"#,
        id,
    )
    .fetch_optional(ex)
    .await
}

pub async fn update_password_hash<'e, E>(ex: E, id: i64, hash: &str) -> Result<(), sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    sqlx::query!("UPDATE users SET password_hash = ? WHERE id = ?", hash, id,)
        .execute(ex)
        .await?;
    Ok(())
}

pub async fn exists<'e, E>(ex: E) -> Result<bool, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    sqlx::query_scalar!(r#"SELECT EXISTS(SELECT 1 FROM users) AS "exists!: bool""#)
        .fetch_one(ex)
        .await
}

pub async fn count<'e, E>(ex: E) -> Result<i64, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    sqlx::query_scalar!(r#"SELECT COUNT(*) AS "count!: i64" FROM users"#)
        .fetch_one(ex)
        .await
}

pub async fn create<'e, E>(
    ex: E,
    email: &str,
    password_hash: &str,
    is_admin: bool,
) -> Result<User, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    let lower = email.to_lowercase();
    sqlx::query_as!(
        User,
        r#"INSERT INTO users (email, password_hash, is_admin)
           VALUES (?1, ?2, ?3)
           RETURNING id AS "id!: i64",
                     email AS "email!: String",
                     password_hash AS "password_hash!: String",
                     is_admin AS "is_admin!: bool",
                     created_at AS "created_at!: PrimitiveDateTime",
                     updated_at AS "updated_at!: PrimitiveDateTime""#,
        lower,
        password_hash,
        is_admin,
    )
    .fetch_one(ex)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::SqlitePool;

    #[sqlx::test]
    async fn exists_returns_false_for_empty_table(pool: SqlitePool) {
        assert!(!exists(&pool).await.expect("exists query"));
    }

    #[sqlx::test]
    async fn exists_returns_true_after_insert(pool: SqlitePool) {
        create(&pool, "a@b.com", "hash", false)
            .await
            .expect("insert");
        assert!(exists(&pool).await.expect("exists query"));
    }

    #[sqlx::test]
    async fn count_returns_zero_for_empty_table(pool: SqlitePool) {
        assert_eq!(count(&pool).await.expect("count query"), 0);
    }

    #[sqlx::test]
    async fn count_returns_n_for_inserted_rows(pool: SqlitePool) {
        create(&pool, "a@b.com", "hash", false)
            .await
            .expect("insert 1");
        assert_eq!(count(&pool).await.expect("count query"), 1);

        create(&pool, "b@c.com", "hash", false)
            .await
            .expect("insert 2");
        assert_eq!(count(&pool).await.expect("count query"), 2);
    }

    #[sqlx::test]
    async fn create_inserts_admin_user(pool: SqlitePool) {
        let user = create(&pool, "admin@example.com", "hash", true)
            .await
            .expect("create admin");
        assert!(user.is_admin, "expected is_admin=true");
        assert_eq!(user.email, "admin@example.com");

        let found = find_by_email(&pool, "admin@example.com")
            .await
            .expect("find_by_email")
            .expect("user exists");
        assert!(found.is_admin);
    }

    #[sqlx::test]
    async fn create_inserts_non_admin_user(pool: SqlitePool) {
        let user = create(&pool, "user@example.com", "hash", false)
            .await
            .expect("create non-admin");
        assert!(!user.is_admin, "expected is_admin=false");
    }

    #[sqlx::test]
    async fn create_lowercases_email(pool: SqlitePool) {
        create(&pool, "Alice@Example.com", "hash", false)
            .await
            .expect("create");

        let found = find_by_email(&pool, "alice@example.com")
            .await
            .expect("find_by_email")
            .expect("user exists");
        assert_eq!(found.email, "alice@example.com");
    }

    /// Email uniqueness constraint is case-insensitive.
    #[sqlx::test]
    async fn email_uniqueness_is_case_insensitive(pool: SqlitePool) {
        create(&pool, "alice@example.com", "hash", false)
            .await
            .expect("seed user");

        let result: Result<i64, _> = sqlx::query_scalar(
            "INSERT INTO users (email, password_hash) VALUES (?1, ?2) RETURNING id",
        )
        .bind("ALICE@example.com")
        .bind("x")
        .fetch_one(&pool)
        .await;

        assert!(
            result.is_err(),
            "case-variant duplicate email must be rejected"
        );
    }
}
