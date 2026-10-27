//! Durable FanDuel→GSIS player links, owned by the nfl-data store.

mod player_link;

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::{SqliteConnection, SqlitePool};
use std::str::FromStr;

use crate::salaries::SalaryError;
pub use player_link::PlayerLink;

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

/// single-connection pool
#[derive(Clone)]
pub(crate) struct Store {
    pool: SqlitePool,
}

impl Store {
    pub(crate) async fn open(database_url: &str) -> Result<Self, SalaryError> {
        if database_url == "sqlite::memory:" || database_url == "sqlite://:memory:" {
            return Self::in_memory().await;
        }
        Self::connect_options(database_url).await
    }

    pub(crate) async fn in_memory() -> Result<Self, SalaryError> {
        Self::connect_options("sqlite::memory:").await
    }

    async fn connect_options(database_url: &str) -> Result<Self, SalaryError> {
        let options = SqliteConnectOptions::from_str(database_url)?
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .foreign_keys(true)
            .busy_timeout(std::time::Duration::from_secs(10));
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await?;

        MIGRATOR.run(&pool).await?;

        Ok(Self { pool })
    }

    /// BEGIN IMMEDIATE transaction: commit on Ok, roll back on Err or drop.
    pub(crate) async fn write_tx<T>(
        &self,
        f: impl AsyncFnOnce(&mut SqliteConnection) -> Result<T, SalaryError>,
    ) -> Result<T, SalaryError> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let value = f(&mut *tx).await?;
        tx.commit().await?;
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
        dotted
            .save_links(&[PlayerLink {
                fd_player_id: 1,
                gsis_player_id: "00-1".into(),
            }])
            .await
            .expect("save");
        let bare = Store::open("sqlite::memory:").await.expect("bare form");
        bare.save_links(&[PlayerLink {
            fd_player_id: 2,
            gsis_player_id: "00-2".into(),
        }])
        .await
        .expect("save");
        assert_eq!(dotted.links().await.unwrap().len(), 1);
        assert_eq!(bare.links().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn write_tx_rolls_back_on_error() {
        let store = store().await;
        let result: Result<(), SalaryError> = store
            .write_tx(async |conn| {
                player_link::save_links_tx(
                    conn,
                    &[PlayerLink {
                        fd_player_id: 1,
                        gsis_player_id: "00-rb".into(),
                    }],
                )
                .await?;
                Err(SalaryError::Schedule)
            })
            .await;
        assert!(result.is_err());
        assert!(
            store.links().await.unwrap().is_empty(),
            "rollback removed rows"
        );
    }
}
