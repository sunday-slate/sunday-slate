use std::sync::atomic::{AtomicU64, Ordering};

use crate::User;
use crate::auth::password;
use crate::users::store as users;
use fake::Fake;
use fake::faker::internet::en::*;
use sqlx::SqlitePool;

/// `users.email` is UNIQUE; faker's `SafeEmail` draws from a small pool, so
/// two default-email users in one test can collide. A per-process sequence in
/// the local part makes every default email distinct.
static EMAIL_SEQ: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Default)]
pub struct UserOptions {
    pub email: Option<String>,
    pub password: Option<String>,
    pub is_admin: bool,
}

#[derive(Debug, Clone)]
pub struct GeneratedUser {
    pub user: User,
    pub password: String,
}

pub async fn user(pool: &SqlitePool, options: UserOptions) -> GeneratedUser {
    let email = options.email.unwrap_or_else(|| {
        let seq = EMAIL_SEQ.fetch_add(1, Ordering::Relaxed);
        let base = SafeEmail().fake::<String>();
        let (local, domain) = base.split_once('@').expect("fake email has a domain");
        format!("{local}-{seq}@{domain}")
    });

    let password = options.password.unwrap_or_else(|| Password(8..16).fake());
    let hash = password::hash(&password);

    let user = users::create(pool, &email, &hash, options.is_admin)
        .await
        .expect("factory: create user");

    GeneratedUser { user, password }
}
