use crate::NflDataError;
use crate::store::Store;
use sqlx::{Sqlite, SqliteConnection};
use std::collections::HashMap;

/// A confirmed link between a FanDuel player and its GSIS identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayerLink {
    pub fd_player_id: u32,
    pub gsis_player_id: String,
}

/// Every persisted link as a FanDuel-id → GSIS-id map.
pub(crate) async fn links<'e, E>(ex: E) -> Result<HashMap<u32, String>, NflDataError>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    let rows = sqlx::query!(
        r#"SELECT fd_player_id AS "fd_player_id: u32",
                      gsis_player_id AS "gsis_player_id: String"
               FROM player_links"#
    )
    .fetch_all(ex)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| (row.fd_player_id, row.gsis_player_id))
        .collect())
}

/// Every linked GSIS id, deduplicated and sorted.
pub(crate) async fn gsis_ids<'e, E>(ex: E) -> Result<Vec<String>, NflDataError>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    let rows = sqlx::query!(
        r#"SELECT DISTINCT gsis_player_id AS "gsis_player_id: String"
               FROM player_links ORDER BY gsis_player_id"#
    )
    .fetch_all(ex)
    .await?;
    Ok(rows.into_iter().map(|row| row.gsis_player_id).collect())
}

/// Upsert each link keyed on the GSIS id; empty set writes nothing. A link
/// is written once: saving an identical pair is a no-op, and a conflicting
/// save (already-linked GSIS id) is ignored. Runs in one BEGIN IMMEDIATE
/// transaction via `Store::write_tx`.
pub(crate) async fn save_links(store: &Store, links: &[PlayerLink]) -> Result<(), sqlx::Error> {
    store
        .write_tx(async |conn| save_links_tx(conn, links).await)
        .await
}

/// Upsert the links on the caller's write connection, inside its
/// transaction. Private: writes only happen via `save_links`, which owns the
/// `write_tx` transaction.
async fn save_links_tx(
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
    use crate::store::Store;

    async fn store() -> Store {
        Store::in_memory().await.expect("in-memory store")
    }

    #[tokio::test]
    async fn links_round_trip_through_the_store() {
        let store = store().await;
        assert!(links(store.reader()).await.unwrap().is_empty());

        save_links(
            &store,
            &[
                PlayerLink {
                    fd_player_id: 166092,
                    gsis_player_id: "00-0036355".into(),
                },
                PlayerLink {
                    fd_player_id: 12543,
                    gsis_player_id: "00-0045678".into(),
                },
            ],
        )
        .await
        .unwrap();

        assert_eq!(
            links(store.reader()).await.unwrap(),
            HashMap::from([
                (166092, "00-0036355".to_owned()),
                (12543, "00-0045678".to_owned()),
            ])
        );
        assert_eq!(
            gsis_ids(store.reader()).await.unwrap(),
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
        save_links(&store, std::slice::from_ref(&link))
            .await
            .unwrap();
        save_links(&store, &[link]).await.unwrap();
        assert_eq!(links(store.reader()).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_conflicting_fd_id_does_not_move_an_existing_gsis_link() {
        let store = store().await;
        save_links(
            &store,
            &[PlayerLink {
                fd_player_id: 111,
                gsis_player_id: "00-x".into(),
            }],
        )
        .await
        .unwrap();
        save_links(
            &store,
            &[PlayerLink {
                fd_player_id: 222,
                gsis_player_id: "00-x".into(),
            }],
        )
        .await
        .unwrap();
        assert_eq!(
            links(store.reader()).await.unwrap(),
            HashMap::from([(111, "00-x".to_owned())])
        );
    }

    #[tokio::test]
    async fn empty_save_writes_nothing() {
        let store = store().await;
        save_links(&store, &[]).await.unwrap();
        assert!(links(store.reader()).await.unwrap().is_empty());
    }
}
