use std::{str::FromStr, time::Duration};

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use tower_sessions::{
    Expiry, SessionManagerLayer,
    cookie::{SameSite, time::Duration as CookieDuration},
};
use tower_sessions_sqlx_store::SqliteStore;

use crate::Config;

pub type Layer = SessionManagerLayer<SqliteStore>;

pub async fn layer(config: &Config) -> anyhow::Result<(Layer, SqliteStore)> {
    build_layer(&config.session_database_url()).await
}

/// Build a session layer backed by an in-memory SQLite database, for tests.
/// The pool below uses a single connection, so the in-memory database lives
/// exactly as long as the returned `SqliteStore` (i.e. the whole test).
pub async fn layer_in_memory() -> anyhow::Result<(Layer, SqliteStore)> {
    build_layer("sqlite::memory:").await
}

async fn build_layer(database_url: &str) -> anyhow::Result<(Layer, SqliteStore)> {
    let options = SqliteConnectOptions::from_str(database_url)?
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .foreign_keys(true)
        .busy_timeout(Duration::from_secs(10));

    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_secs(30))
        .connect_with(options)
        .await?;

    let store = SqliteStore::new(pool);
    store.migrate().await?;

    let session_layer = SessionManagerLayer::new(store.clone())
        .with_secure(false)
        .with_http_only(true)
        .with_same_site(SameSite::Lax)
        .with_expiry(Expiry::OnInactivity(CookieDuration::days(14)));

    Ok((session_layer, store))
}
