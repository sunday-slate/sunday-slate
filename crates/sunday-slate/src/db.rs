use std::{str::FromStr, time::Duration};

use sqlx::{
    migrate::Migrator,
    sqlite::{
        SqliteConnectOptions, SqliteConnection, SqliteJournalMode, SqlitePool, SqlitePoolOptions,
        SqliteSynchronous,
    },
};

static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

#[derive(Clone)]
pub struct Db {
    read: SqlitePool,
    write: SqlitePool,
}

impl Db {
    /// Runs the crate's migrations against a caller-built test pool.
    #[cfg(any(feature = "test-support", test))]
    pub async fn migrate_pool(pool: &SqlitePool) -> Result<(), sqlx::migrate::MigrateError> {
        MIGRATOR.run(pool).await
    }

    pub async fn open(path: &str) -> anyhow::Result<Self> {
        let base = SqliteConnectOptions::from_str(path)?
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .foreign_keys(true)
            .busy_timeout(Duration::from_secs(10))
            .optimize_on_close(true, None)
            .pragma("cache_size", "-20000")
            .pragma("temp_store", "memory")
            .pragma("mmap_size", "134217728")
            .pragma("journal_size_limit", "67108864");

        let write = SqlitePoolOptions::new()
            .max_connections(1)
            .acquire_timeout(Duration::from_secs(30))
            .connect_with(base.clone())
            .await?;

        MIGRATOR.run(&write).await?;

        let read = SqlitePoolOptions::new()
            .max_connections(8)
            .acquire_timeout(Duration::from_secs(5))
            .connect_with(base.read_only(true))
            .await?;

        Ok(Self { read, write })
    }

    /// Returns the read pool.
    pub fn reader(&self) -> &SqlitePool {
        &self.read
    }

    /// Run a write inside a BEGIN IMMEDIATE transaction. Commits on Ok, rolls back on Err.
    pub async fn write_tx<F, T, E>(&self, f: F) -> Result<T, E>
    where
        F: for<'c> AsyncFnOnce(&'c mut SqliteConnection) -> Result<T, E>,
        E: From<sqlx::Error>,
    {
        let mut conn = self.write.acquire().await.map_err(E::from)?;
        sqlx::query("BEGIN IMMEDIATE")
            .execute(&mut *conn)
            .await
            .map_err(E::from)?;

        match f(&mut *conn).await {
            Ok(v) => {
                sqlx::query("COMMIT")
                    .execute(&mut *conn)
                    .await
                    .map_err(E::from)?;
                Ok(v)
            }
            Err(e) => {
                let _ = sqlx::query("ROLLBACK").execute(&mut *conn).await;
                Err(e)
            }
        }
    }

    /// Test-only constructor: `read` and `write` both alias the same pool.
    /// The production read/write split and `max_connections=1` write
    /// serialization do not apply. Tests own their database, so
    /// serialization is unnecessary.
    ///
    /// Tests that need concurrent writes (multiple in-flight `oneshot`
    /// requests hitting handlers that write) must NOT use this — they
    /// need a real production-shaped `Db`. No existing tests do that.
    #[cfg(any(feature = "test-support", test))]
    pub fn test(pool: SqlitePool) -> Self {
        Self {
            read: pool.clone(),
            write: pool,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[sqlx::test]
    async fn test_db_test_constructor(pool: SqlitePool) {
        let db = Db::test(pool.clone());
        // Both reader and writer pool should serve the same underlying database
        let mut write_conn = db.write.acquire().await.unwrap();
        sqlx::query("CREATE TABLE IF NOT EXISTS widgets (id INTEGER PRIMARY KEY)")
            .execute(&mut *write_conn)
            .await
            .unwrap();

        // Read through the reader pool — the table should be visible
        let mut read_conn = db.reader().acquire().await.unwrap();
        let tables: Vec<(String,)> =
            sqlx::query_as("SELECT name FROM sqlite_master WHERE type='table' AND name='widgets'")
                .fetch_all(&mut *read_conn)
                .await
                .unwrap();
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].0, "widgets");
    }
}
