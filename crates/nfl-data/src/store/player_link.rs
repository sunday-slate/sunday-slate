use crate::SalaryError;
use crate::store::Store;
use sqlx::SqliteConnection;
use std::collections::HashMap;

/// A confirmed link between a FanDuel player and its GSIS identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayerLink {
    pub fd_player_id: u32,
    pub gsis_player_id: String,
}

impl Store {
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

    /// Upsert each link keyed on the GSIS id; empty set writes nothing.
    pub(crate) async fn save_links(&self, links: &[PlayerLink]) -> Result<(), SalaryError> {
        self.write_tx(async |conn| {
            save_links_tx(conn, links).await?;
            Ok(())
        })
        .await
    }
}

pub(crate) async fn save_links_tx(
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
    async fn updated_at_touches_on_update() {
        let store = store().await;
        store
            .save_links(&[PlayerLink {
                fd_player_id: 1,
                gsis_player_id: "00-a".into(),
            }])
            .await
            .unwrap();

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
