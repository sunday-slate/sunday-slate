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
    /// A link is written once: saving an identical pair is a no-op, and a
    /// conflicting save (already-linked GSIS id) is ignored.
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
               ON CONFLICT (gsis_player_id) DO NOTHING"#,
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
        assert!(store.links().await.unwrap().is_empty());

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

        assert_eq!(
            store.links().await.unwrap(),
            HashMap::from([
                (166092, "00-0036355".to_owned()),
                (12543, "00-0045678".to_owned()),
            ])
        );
        assert_eq!(
            store.gsis_ids().await.unwrap(),
            vec!["00-0036355".to_owned(), "00-0045678".to_owned()]
        );
    }

    #[tokio::test]
    async fn saving_the_same_link_twice_is_idempotent() {
        let store = store().await;
        let link = PlayerLink {
            fd_player_id: 166092,
            gsis_player_id: "00-0036355".into(),
        };
        store.save_links(std::slice::from_ref(&link)).await.unwrap();
        store.save_links(&[link]).await.unwrap();
        assert_eq!(store.links().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_conflicting_fd_id_does_not_move_an_existing_gsis_link() {
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
        assert_eq!(
            store.links().await.unwrap(),
            HashMap::from([(111, "00-x".to_owned())])
        );
    }

    #[tokio::test]
    async fn empty_save_writes_nothing() {
        let store = store().await;
        store.save_links(&[]).await.unwrap();
        assert!(store.links().await.unwrap().is_empty());
    }
}
