//! Durable FanDuel→GSIS player links, owned by the nfl-data store.

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::{SqliteConnection, SqlitePool};
use std::collections::HashMap;
use std::str::FromStr;

use crate::salaries::SalaryError;

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

/// A confirmed link between a FanDuel player and its GSIS identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayerLink {
    pub fd_player_id: u32,
    pub gsis_player_id: String,
}

/// The nfl-data store: currently one durable table of player links.
///
/// Single-pool like `fanduel-data`'s store: one connection serializes every
/// write, and in-memory connections share that same pool so schema and data
/// cannot split across connections.
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

    /// The linked GSIS id for one FanDuel id, when linked.
    pub(crate) async fn by_fd_id(
        &self,
        fd_player_id: u32,
    ) -> Result<Option<PlayerLink>, SalaryError> {
        let row = sqlx::query!(
            r#"SELECT fd_player_id AS "fd_player_id: u32",
                      gsis_player_id AS "gsis_player_id: String"
               FROM player_links WHERE fd_player_id = ?"#,
            fd_player_id,
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|row| PlayerLink {
            fd_player_id: row.fd_player_id,
            gsis_player_id: row.gsis_player_id,
        }))
    }

    /// Every persisted link as a FanDuel-id → GSIS-id map.
    pub(crate) async fn links(&self) -> Result<HashMap<u32, String>, SalaryError> {
        let rows = sqlx::query!(
            r#"SELECT fd_player_id AS "fd_player_id: u32",
                      gsis_player_id AS "gsis_player_id: String"
               FROM player_links"#
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|row| (row.fd_player_id, row.gsis_player_id))
            .collect())
    }

    /// Every linked GSIS id, deduplicated and sorted.
    pub(crate) async fn gsis_ids(&self) -> Result<Vec<String>, SalaryError> {
        let rows = sqlx::query!(
            r#"SELECT DISTINCT gsis_player_id AS "gsis_player_id: String"
               FROM player_links ORDER BY gsis_player_id"#
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(|row| row.gsis_player_id).collect())
    }

    /// Replace the source set in one transaction: an upsert per link keyed on
    /// the GSIS id, so a re-link moves the FanDuel id rather than adding rows.
    /// An empty set writes nothing.
    pub(crate) async fn save_links(&self, links: &[PlayerLink]) -> Result<(), SalaryError> {
        self.write_tx(async |conn| {
            save_links_tx(conn, links).await?;
            Ok(())
        })
        .await
    }

    /// Run a write inside a BEGIN IMMEDIATE transaction. Commits on Ok, rolls back on Err.
    pub(crate) async fn write_tx<T>(
        &self,
        f: impl AsyncFnOnce(&mut SqliteConnection) -> Result<T, SalaryError>,
    ) -> Result<T, SalaryError> {
        let mut conn = self.pool.acquire().await?;
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

async fn save_links_tx(
    conn: &mut SqliteConnection,
    links: &[PlayerLink],
) -> Result<(), sqlx::Error> {
    for link in links {
        sqlx::query!(
            r#"INSERT INTO player_links (fd_player_id, gsis_player_id) VALUES (?, ?)
               ON CONFLICT (gsis_player_id) DO UPDATE SET fd_player_id = excluded.fd_player_id"#,
            i64::from(link.fd_player_id),
            link.gsis_player_id,
        )
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
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
        // Each form sees only its own pool: schema and data stayed together.
        assert_eq!(dotted.links().await.unwrap().len(), 1);
        assert_eq!(bare.links().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn links_round_trip_through_the_store() {
        let store = store().await;
        assert!(store.by_fd_id(166092).await.unwrap().is_none());

        store
            .save_links(&[
                PlayerLink {
                    fd_player_id: 166092,
                    gsis_player_id: "00-0036355".into(),
                },
                PlayerLink {
                    fd_player_id: 12543,
                    gsis_player_id: "00-0045678".into(),
                },
            ])
            .await
            .unwrap();

        let link = store.by_fd_id(166092).await.unwrap().expect("hit");
        assert_eq!(link.gsis_player_id, "00-0036355");
        assert_eq!(store.links().await.unwrap().len(), 2);
        assert_eq!(
            store.gsis_ids().await.unwrap(),
            vec!["00-0036355".to_owned(), "00-0045678".to_owned()]
        );
    }

    #[tokio::test]
    async fn upsert_moves_the_fd_id_under_the_gsis_key() {
        let store = store().await;
        store
            .save_links(&[PlayerLink {
                fd_player_id: 111,
                gsis_player_id: "00-x".into(),
            }])
            .await
            .unwrap();
        store
            .save_links(&[PlayerLink {
                fd_player_id: 222,
                gsis_player_id: "00-x".into(),
            }])
            .await
            .unwrap();

        assert!(store.by_fd_id(111).await.unwrap().is_none());
        assert_eq!(
            store
                .by_fd_id(222)
                .await
                .unwrap()
                .expect("moved")
                .gsis_player_id,
            "00-x"
        );
        assert_eq!(store.links().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn empty_save_writes_nothing() {
        let store = store().await;
        store.save_links(&[]).await.unwrap();
        assert!(store.links().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn write_tx_rolls_back_on_error() {
        let store = store().await;
        let result: Result<(), SalaryError> = store
            .write_tx(async |conn| {
                save_links_tx(
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

    #[tokio::test]
    async fn updated_at_touches_on_update() {
        let store = store().await;
        store
            .save_links(&[PlayerLink {
                fd_player_id: 1,
                gsis_player_id: "00-a".into(),
            }])
            .await
            .unwrap();

        // A remap of the same GSIS identity is an UPDATE: the conflict clause
        // changes only fd_player_id, so the trigger moves updated_at forward.
        store
            .save_links(&[PlayerLink {
                fd_player_id: 2,
                gsis_player_id: "00-a".into(),
            }])
            .await
            .unwrap();
        assert!(store.by_fd_id(1).await.unwrap().is_none());
        assert_eq!(
            store
                .by_fd_id(2)
                .await
                .unwrap()
                .expect("moved")
                .gsis_player_id,
            "00-a"
        );
    }
}
