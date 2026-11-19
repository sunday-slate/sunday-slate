use crate::NflDataError;
use crate::store::Store;
use nfl_model::NflPlayerId;
use sqlx::{Sqlite, SqliteConnection};
use std::collections::{HashMap, HashSet};

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

pub(crate) async fn count_missing_espn_ids(store: &Store) -> Result<i64, NflDataError> {
    Ok(sqlx::query_scalar!(
        r#"SELECT COUNT(*) AS "count!: i64" FROM player_links WHERE espn_player_id IS NULL"#
    )
    .fetch_one(store.reader())
    .await?)
}

/// GSIS ids of every saved link without an ESPN id, sorted.
pub(crate) async fn gsis_ids_missing_espn(store: &Store) -> Result<Vec<NflPlayerId>, NflDataError> {
    let rows = sqlx::query!(
        r#"SELECT gsis_player_id AS "gsis_player_id!: NflPlayerId"
               FROM player_links WHERE espn_player_id IS NULL ORDER BY gsis_player_id"#
    )
    .fetch_all(store.reader())
    .await?;
    Ok(rows.into_iter().map(|row| row.gsis_player_id).collect())
}

/// Fill only unambiguous claims on blank links and return the number changed.
pub(crate) async fn update_espn_ids(
    store: &Store,
    updates: &HashMap<NflPlayerId, String>,
) -> Result<u64, NflDataError> {
    if updates.is_empty() {
        return Ok(0);
    }
    store
        .write_tx(async |conn| {
            let rows = sqlx::query!(
                r#"SELECT gsis_player_id AS "gsis_player_id!: String",
                          espn_player_id AS "espn_player_id?: String"
                   FROM player_links"#
            )
            .fetch_all(&mut *conn)
            .await?;
            let blank: HashSet<&str> = rows
                .iter()
                .filter(|row| row.espn_player_id.is_none())
                .map(|row| row.gsis_player_id.as_str())
                .collect();
            let occupied: HashSet<&str> = rows
                .iter()
                .filter_map(|row| row.espn_player_id.as_deref())
                .collect();
            // Count claims against the pre-write state so no competing claim wins by order.
            let eligible = |gsis: &str, espn: &str| blank.contains(gsis) && !occupied.contains(espn);
            let mut claims: HashMap<&str, usize> = HashMap::new();
            for (gsis_player_id, espn_player_id) in updates {
                let gsis = gsis_player_id.0.as_str();
                let espn = espn_player_id.as_str();
                if eligible(gsis, espn) {
                    *claims.entry(espn).or_default() += 1;
                }
            }
            let mut updated = 0u64;
            for (gsis_player_id, espn_player_id) in updates {
                let gsis = gsis_player_id.0.as_str();
                let espn = espn_player_id.as_str();
                if !eligible(gsis, espn) || claims.get(espn).copied() != Some(1) {
                    continue;
                }
                let result = sqlx::query!(
                    "UPDATE player_links SET espn_player_id = ? WHERE gsis_player_id = ? AND espn_player_id IS NULL",
                    espn,
                    gsis,
                )
                .execute(&mut *conn)
                .await?;
                updated += result.rows_affected();
            }
            Ok(updated)
        })
        .await
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
                link(166092, "00-0036355", None),
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
        assert_eq!(
            links(store.reader()).await.unwrap(),
            HashMap::from([
                (166092, "00-0036355".to_owned()),
                (12543, "00-0045678".to_owned()),
                (111, "00-0011111".to_owned()),
            ]),
            "rows without a usable ESPN id still link"
        );
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
    async fn a_repeated_gsis_save_keeps_the_original_espn_id() {
        let store = store().await;
        save_links(&store, &[link(166092, "00-0036355", Some("4362628"))])
            .await
            .unwrap();
        save_links(&store, &[link(166092, "00-0036355", Some("9999999"))])
            .await
            .unwrap();

        assert_eq!(
            links(store.reader()).await.unwrap(),
            HashMap::from([(166092, "00-0036355".to_owned())])
        );
        assert_eq!(
            links_by_espn(store.reader()).await.unwrap(),
            HashMap::from([("4362628".to_owned(), "00-0036355".to_owned())]),
            "the GSIS conflict is ignored instead of updating the stored ESPN id"
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
    }

    #[tokio::test]
    async fn espn_fill_skips_stale_and_conflicting_claims() {
        let store = store().await;
        save_links(
            &store,
            &[
                link(1, "00-stale", Some("999")),
                link(2, "00-owner", Some("888")),
                link(3, "00-a", None),
                link(4, "00-b", None),
                link(5, "00-taken", None),
                link(6, "00-good", None),
            ],
        )
        .await
        .unwrap();

        let updates = HashMap::from([
            (NflPlayerId("00-stale".into()), "123".to_owned()),
            (NflPlayerId("00-a".into()), "777".to_owned()),
            (NflPlayerId("00-b".into()), "777".to_owned()),
            (NflPlayerId("00-taken".into()), "888".to_owned()),
            (NflPlayerId("00-good".into()), "123".to_owned()),
            (NflPlayerId("00-missing".into()), "123".to_owned()),
        ]);
        assert_eq!(
            update_espn_ids(&store, &updates).await.unwrap(),
            1,
            "only the unambiguous, blank, untaken claim writes"
        );
        assert_eq!(
            links_by_espn(store.reader()).await.unwrap(),
            HashMap::from([
                ("999".to_owned(), "00-stale".to_owned()),
                ("888".to_owned(), "00-owner".to_owned()),
                ("123".to_owned(), "00-good".to_owned()),
            ])
        );
        assert_eq!(update_espn_ids(&store, &updates).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn empty_save_writes_nothing() {
        let store = store().await;
        save_links(&store, &[]).await.unwrap();
        assert!(links(store.reader()).await.unwrap().is_empty());
    }
}
