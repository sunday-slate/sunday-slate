use crate::NflDataError;
use crate::store::Store;
use sqlx::{Sqlite, SqliteConnection};
use std::collections::HashMap;

/// A confirmed link between a FanDuel player and its GSIS identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlayerLink {
    pub fd_player_id: u32,
    pub gsis_player_id: String,
    pub espn_player_id: Option<String>,
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

/// Every persisted link whose FanDuel id is in `fd_player_ids`, as a
/// FanDuel-id → GSIS-id map; an empty set reads nothing.
pub(crate) async fn links_for_fd_ids<'e, E>(
    ex: E,
    fd_player_ids: &[u32],
) -> Result<HashMap<u32, String>, NflDataError>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    if fd_player_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let mut builder = sqlx::QueryBuilder::<Sqlite>::new(
        r#"SELECT fd_player_id, gsis_player_id
               FROM player_links
               WHERE fd_player_id IN ("#,
    );
    let mut ids = builder.separated(", ");
    for fd_player_id in fd_player_ids {
        ids.push_bind(i64::from(*fd_player_id));
    }
    ids.push_unseparated(")");
    let rows = builder
        .build_query_as::<(i64, String)>()
        .fetch_all(ex)
        .await?;
    Ok(rows
        .into_iter()
        .map(|(fd_player_id, gsis_player_id)| (fd_player_id as u32, gsis_player_id))
        .collect())
}

/// Every persisted ESPN→GSIS identity link. This is the only mapping the
/// injury path uses; rows without a stored ESPN id are absent.
pub(crate) async fn links_by_espn<'e, E>(ex: E) -> Result<HashMap<String, String>, NflDataError>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    let rows = sqlx::query!(
        r#"SELECT espn_player_id AS "espn_player_id!: String",
                  gsis_player_id AS "gsis_player_id: String"
               FROM player_links
               WHERE espn_player_id IS NOT NULL"#
    )
    .fetch_all(ex)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| (row.espn_player_id, row.gsis_player_id))
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
        // Whitespace-only source ids carry no identity.
        let espn_player_id = link
            .espn_player_id
            .as_deref()
            .map(str::trim)
            .filter(|id| !id.is_empty());
        sqlx::query!(
            r#"INSERT INTO player_links (fd_player_id, gsis_player_id, espn_player_id)
               VALUES (?, ?, ?)
               ON CONFLICT (gsis_player_id) DO NOTHING"#,
            i64::from(link.fd_player_id),
            link.gsis_player_id,
            espn_player_id,
        )
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}

/// Persist FanDuel→GSIS player links, skipping an empty batch. Callers
/// republish the snapshot at their workflow boundary, once the whole import is
/// persisted.
pub(crate) async fn save_player_links(
    store: &Store,
    links: &[PlayerLink],
) -> Result<(), NflDataError> {
    if links.is_empty() {
        return Ok(());
    }
    save_links(store, links).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;

    async fn store() -> Store {
        Store::in_memory().await.expect("in-memory store")
    }

    fn link(fd_player_id: u32, gsis_player_id: &str, espn_player_id: Option<&str>) -> PlayerLink {
        PlayerLink {
            fd_player_id,
            gsis_player_id: gsis_player_id.into(),
            espn_player_id: espn_player_id.map(str::to_owned),
        }
    }

    #[tokio::test]
    async fn links_round_trip_through_the_store() {
        let store = store().await;
        assert!(links(store.reader()).await.unwrap().is_empty());

        save_links(
            &store,
            &[
                link(166092, "00-0036355", Some("4362628")),
                link(12543, "00-0045678", None),
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
    async fn espn_links_round_trip_and_missing_ids_coexist() {
        let store = store().await;
        save_links(
            &store,
            &[
                link(166092, "00-0036355", Some("4362628")),
                link(12543, "00-0045678", None),
                link(111, "00-0011111", Some("  ")),
            ],
        )
        .await
        .unwrap();

        assert_eq!(
            links_by_espn(store.reader()).await.unwrap(),
            HashMap::from([("4362628".to_owned(), "00-0036355".to_owned())]),
            "only stored ESPN ids map, and a whitespace id stores as null"
        );
        assert_eq!(links(store.reader()).await.unwrap().len(), 3);
    }

    #[tokio::test]
    async fn links_for_fd_ids_only_returns_the_requested_links() {
        let store = store().await;
        save_links(
            &store,
            &[
                link(166092, "00-0036355", None),
                link(12543, "00-0045678", None),
            ],
        )
        .await
        .unwrap();

        assert_eq!(
            links_for_fd_ids(store.reader(), &[166092, 404])
                .await
                .unwrap(),
            HashMap::from([(166092, "00-0036355".to_owned())])
        );
        assert_eq!(
            links_for_fd_ids(store.reader(), &[166092, 12543])
                .await
                .unwrap()
                .len(),
            2
        );
        assert!(
            links_for_fd_ids(store.reader(), &[])
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn saving_the_same_link_twice_is_idempotent() {
        let store = store().await;
        let link = link(166092, "00-0036355", Some("4362628"));
        save_links(&store, std::slice::from_ref(&link))
            .await
            .unwrap();
        save_links(&store, &[link]).await.unwrap();
        assert_eq!(links(store.reader()).await.unwrap().len(), 1);
        assert_eq!(
            links_by_espn(store.reader()).await.unwrap().len(),
            1,
            "an identical save leaves the stored ESPN id alone"
        );
    }

    #[tokio::test]
    async fn a_conflicting_fd_id_does_not_move_an_existing_gsis_link() {
        let store = store().await;
        save_links(&store, &[link(111, "00-x", None)])
            .await
            .unwrap();
        save_links(&store, &[link(222, "00-x", None)])
            .await
            .unwrap();
        assert_eq!(
            links(store.reader()).await.unwrap(),
            HashMap::from([(111, "00-x".to_owned())])
        );
    }

    #[tokio::test]
    async fn a_conflicting_espn_id_rolls_back_the_batch() {
        let store = store().await;
        let error = save_links(
            &store,
            &[
                link(111, "00-a", Some("999")),
                link(222, "00-b", Some("999")),
            ],
        )
        .await
        .expect_err("one ESPN id cannot map to two GSIS ids");
        assert!(
            error.to_string().contains("UNIQUE"),
            "the ESPN index rejects the second row: {error}"
        );
        assert!(
            links(store.reader()).await.unwrap().is_empty(),
            "the whole batch rolled back, including the first insert"
        );
        assert!(links_by_espn(store.reader()).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn empty_save_writes_nothing() {
        let store = store().await;
        save_links(&store, &[]).await.unwrap();
        assert!(links(store.reader()).await.unwrap().is_empty());
    }
}
