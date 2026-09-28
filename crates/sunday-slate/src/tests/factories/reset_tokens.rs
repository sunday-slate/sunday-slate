use crate::User;
use crate::auth::password;
use crate::auth::store as reset_tokens;
use crate::tests::factories;
use crate::tests::factories::UserOptions;
use sqlx::SqlitePool;
use uuid::Uuid;

#[derive(Debug, Clone, Default)]
pub struct ResetTokenOptions {
    pub user: Option<User>,
}

#[derive(Debug, Clone)]
pub struct GeneratedResetToken {
    pub id: i64,
    pub user: User,
    secret: String,
}

impl GeneratedResetToken {
    pub fn token(&self) -> String {
        format!("{}.{}", self.id, self.secret)
    }
}

pub async fn reset_token(pool: &SqlitePool, options: ResetTokenOptions) -> GeneratedResetToken {
    let user = match options.user {
        Some(u) => u,
        None => factories::user(pool, UserOptions::default()).await.user,
    };

    let secret = Uuid::new_v4().to_string();
    let hash = password::hash(&secret);

    let id = reset_tokens::insert_token(pool, user.id, &hash)
        .await
        .expect("factory: insert reset token");

    GeneratedResetToken { id, user, secret }
}

pub async fn expired_reset_token(
    pool: &SqlitePool,
    options: ResetTokenOptions,
) -> GeneratedResetToken {
    let token = reset_token(pool, options).await;

    sqlx::query("UPDATE password_reset_tokens SET expires_at = datetime('now','subsec','-1 hour') WHERE id = ?")
        .bind(token.id)
        .execute(pool)
        .await
        .expect("factory: expire reset token");

    token
}
