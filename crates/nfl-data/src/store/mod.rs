//! Durable FanDuel→GSIS player links, owned by the nfl-data store.

pub(crate) mod player_link;

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::{SqliteConnection, SqlitePool};
use std::str::FromStr;
use std::time::Duration;

use crate::salaries::SalaryError;
pub use player_link::PlayerLink;

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

/// A read-only reader pool and a single-connection writer pool for file
/// databases: writes only happen inside `write_tx` on the writer, and the
/// reader rejects writes at the database level. Fresh in-memory stores (test
/// only) alias one shared pool instead, like sunday-slate's `Db::test`.
#[derive(Clone)]
pub(crate) struct Store {
    read: SqlitePool,
    write: SqlitePool,
}

impl Store {
    pub(crate) async fn open(database_url: &str) -> Result<Self, SalaryError> {
        if database_url == "sqlite::memory:" || database_url == "sqlite://:memory:" {
            return Self::in_memory().await;
        }
        Self::connect_options(database_url).await
    }

    /// Fresh, empty, test-only store: one shared in-memory pool serving both
    /// reads and writes. Separate `:memory:` connections would be distinct
    /// databases, so — like sunday-slate's `Db::test` — both accessors alias
    /// the same pool and the reader/writer split does not apply here.
    pub(crate) async fn in_memory() -> Result<Self, SalaryError> {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(Self::options("sqlite::memory:")?)
            .await?;

        MIGRATOR.run(&pool).await?;

        Ok(Self {
            read: pool.clone(),
            write: pool,
        })
    }

    async fn connect_options(database_url: &str) -> Result<Self, SalaryError> {
        let write = SqlitePoolOptions::new()
            .max_connections(1)
            .acquire_timeout(Duration::from_secs(30))
            .connect_with(Self::options(database_url)?)
            .await?;

        MIGRATOR.run(&write).await?;

        let read = SqlitePoolOptions::new()
            .max_connections(8)
            .acquire_timeout(Duration::from_secs(5))
            .connect_with(Self::options(database_url)?.read_only(true))
            .await?;

        Ok(Self { read, write })
    }

    fn options(database_url: &str) -> Result<SqliteConnectOptions, SalaryError> {
        Ok(SqliteConnectOptions::from_str(database_url)?
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .foreign_keys(true)
            .busy_timeout(Duration::from_secs(10)))
    }

    /// Read-only executor for the store's read functions. Writes are only
    /// possible through `write_tx` (via `player_link::save_links`).
    pub(crate) fn reader(&self) -> &SqlitePool {
        &self.read
    }

    /// Run a write inside a BEGIN IMMEDIATE transaction on the writer pool.
    /// Commits on Ok, rolls back on Err or drop. Generic over the caller's
    /// error type, like sunday-slate's `Db::write_tx`; store functions return
    /// low-level `sqlx::Error` and the caller converts.
    pub(crate) async fn write_tx<F, T, E>(&self, f: F) -> Result<T, E>
    where
        F: for<'c> AsyncFnOnce(&'c mut SqliteConnection) -> Result<T, E>,
        E: From<sqlx::Error>,
    {
        let mut tx = self
            .write
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(E::from)?;
        let value = f(&mut *tx).await?;
        tx.commit().await.map_err(E::from)?;
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn store() -> Store {
        Store::in_memory().await.expect("in-memory store")
    }

    #[tokio::test]
    async fn in_memory_recognizes_both_url_forms() {
        let dotted = Store::open("sqlite://:memory:").await.expect("dotted form");
        player_link::save_links(
            &dotted,
            &[PlayerLink {
                fd_player_id: 1,
                gsis_player_id: "00-1".into(),
            }],
        )
        .await
        .expect("save");
        let bare = Store::open("sqlite::memory:").await.expect("bare form");
        player_link::save_links(
            &bare,
            &[PlayerLink {
                fd_player_id: 2,
                gsis_player_id: "00-2".into(),
            }],
        )
        .await
        .expect("save");
        assert_eq!(player_link::links(dotted.reader()).await.unwrap().len(), 1);
        assert_eq!(player_link::links(bare.reader()).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn write_tx_rolls_back_on_error() {
        let store = store().await;
        let result: Result<(), SalaryError> = store
            .write_tx(async |conn| {
                sqlx::query(
                    "INSERT INTO player_links (fd_player_id, gsis_player_id) VALUES (1, '00-rb')",
                )
                .execute(conn)
                .await?;
                Err(SalaryError::Schedule)
            })
            .await;
        assert!(result.is_err());
        assert!(
            player_link::links(store.reader()).await.unwrap().is_empty(),
            "rollback removed rows"
        );
    }

    #[tokio::test]
    async fn the_reader_pool_rejects_writes() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("store.db");
        let store = Store::open(&format!("sqlite:{}", path.display()))
            .await
            .expect("file store");

        // The write path works and leaves the schema and WAL ready for
        // readers.
        player_link::save_links(
            &store,
            &[PlayerLink {
                fd_player_id: 1,
                gsis_player_id: "00-1".into(),
            }],
        )
        .await
        .expect("save");

        let direct = sqlx::query(
            "INSERT INTO player_links (fd_player_id, gsis_player_id) VALUES (2, '00-2')",
        )
        .execute(store.reader())
        .await;
        let err = direct.expect_err("writes outside write_tx must fail");
        assert!(
            err.to_string().to_lowercase().contains("readonly"),
            "expected a read-only rejection, got: {err}"
        );
        assert_eq!(
            player_link::links(store.reader()).await.unwrap().len(),
            1,
            "the direct write did not land"
        );
    }
}
