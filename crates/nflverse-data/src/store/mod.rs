pub(crate) mod games;
pub(crate) mod player_stats;
pub(crate) mod players;
pub(crate) mod rosters;
pub(crate) mod sync_state;
pub(crate) mod team_stats;
pub(crate) mod weekly_rosters;

use std::{str::FromStr, time::Duration};

use sqlx::{
    migrate::Migrator,
    sqlite::{
        SqliteConnectOptions, SqliteConnection, SqliteJournalMode, SqlitePool, SqlitePoolOptions,
        SqliteSynchronous,
    },
};

use crate::error::NflDataError;

static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

pub(crate) struct Store {
    read: SqlitePool,
    write: SqlitePool,
}

impl Store {
    pub(crate) async fn open(database_url: &str) -> Result<Self, NflDataError> {
        let base = SqliteConnectOptions::from_str(database_url)?
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .foreign_keys(true)
            .busy_timeout(Duration::from_secs(10))
            .optimize_on_close(true, None);

        let write = SqlitePoolOptions::new()
            .max_connections(1)
            .acquire_timeout(Duration::from_secs(30))
            .connect_with(base.clone())
            .await?;

        MIGRATOR.run(&write).await?;

        let read = SqlitePoolOptions::new()
            .max_connections(4)
            .acquire_timeout(Duration::from_secs(5))
            .connect_with(base.read_only(true))
            .await?;

        Ok(Self { read, write })
    }

    pub(crate) async fn in_memory() -> Result<Self, NflDataError> {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await?;
        MIGRATOR.run(&pool).await?;
        Ok(Self {
            read: pool.clone(),
            write: pool,
        })
    }

    pub(crate) fn reader(&self) -> &SqlitePool {
        &self.read
    }

    /// Run a write inside a BEGIN IMMEDIATE transaction. Commits on Ok, rolls back on Err.
    pub(crate) async fn write_tx<T>(
        &self,
        f: impl AsyncFnOnce(&mut SqliteConnection) -> Result<T, NflDataError>,
    ) -> Result<T, NflDataError> {
        let mut conn = self.write.acquire().await?;
        sqlx::query("BEGIN IMMEDIATE").execute(&mut *conn).await?;

        match f(&mut *conn).await {
            Ok(v) => {
                sqlx::query("COMMIT").execute(&mut *conn).await?;
                Ok(v)
            }
            Err(e) => {
                let _ = sqlx::query("ROLLBACK").execute(&mut *conn).await;
                Err(e)
            }
        }
    }
}
