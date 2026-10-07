use sqlx::{SqliteConnection, SqlitePool};

use crate::error::NflDataError;
use crate::model::{Player, TeamAbbr};
use crate::store::Store;
use crate::store::sync_state::{self, SyncedAsset};

/// Insert players directly, bypassing the sync pipeline and `sync_state`
/// bookkeeping. Runs inside a caller-supplied transaction. For tests and
/// tooling only — see `NflData::seed_for_test`.
pub(crate) async fn seed(
    conn: &mut SqliteConnection,
    players: &[Player],
) -> Result<(), NflDataError> {
    for p in players {
        sqlx::query!(
            r#"INSERT INTO players
               (gsis_id, espn_id, full_name, first_name, last_name, position,
                latest_team, headshot_url)
               VALUES (?, ?, ?, ?, ?, ?, ?, ?)"#,
            p.gsis_id,
            p.espn_id,
            p.full_name,
            p.first_name,
            p.last_name,
            p.position,
            p.latest_team,
            p.headshot_url
        )
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}

pub(crate) async fn replace(
    store: &Store,
    asset: &SyncedAsset<'_>,
    players: &[Player],
) -> Result<u64, NflDataError> {
    store
        .write_tx(async |conn| {
            sqlx::query!("DELETE FROM players")
                .execute(&mut *conn)
                .await?;
            for p in players {
                sqlx::query!(
                    r#"INSERT INTO players
                       (gsis_id, espn_id, full_name, first_name, last_name, position,
                        latest_team, headshot_url)
                       VALUES (?, ?, ?, ?, ?, ?, ?, ?)"#,
                    p.gsis_id,
                    p.espn_id,
                    p.full_name,
                    p.first_name,
                    p.last_name,
                    p.position,
                    p.latest_team,
                    p.headshot_url
                )
                .execute(&mut *conn)
                .await?;
            }
            sync_state::record(conn, asset).await?;
            Ok(players.len() as u64)
        })
        .await
}

pub(crate) async fn all(pool: &SqlitePool) -> Result<Vec<Player>, NflDataError> {
    Ok(sqlx::query_as!(
        Player,
        r#"SELECT
               gsis_id,
               espn_id,
               full_name,
               first_name,
               last_name,
               position,
               latest_team AS "latest_team: TeamAbbr",
               headshot_url
           FROM players
           ORDER BY full_name, gsis_id"#
    )
    .fetch_all(pool)
    .await?)
}

pub(crate) async fn by_gsis(
    pool: &SqlitePool,
    gsis_id: &str,
) -> Result<Option<Player>, NflDataError> {
    Ok(sqlx::query_as!(
        Player,
        r#"SELECT
               gsis_id,
               espn_id,
               full_name,
               first_name,
               last_name,
               position,
               latest_team AS "latest_team: TeamAbbr",
               headshot_url
           FROM players
           WHERE gsis_id = ?"#,
        gsis_id
    )
    .fetch_optional(pool)
    .await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn replace_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&format!("sqlite://{}/t.db", dir.path().display()))
            .await
            .unwrap();
        let asset = SyncedAsset {
            release_tag: "players",
            asset_name: "players.csv",
            updated_at: "2026-07-01T11:22:19Z",
        };
        let player = Player {
            gsis_id: "00-0036264".into(),
            full_name: "Jordan Love".into(),
            first_name: Some("Jordan".into()),
            last_name: Some("Love".into()),
            position: Some("QB".into()),
            latest_team: Some(TeamAbbr("GB".into())),
            espn_id: Some("4036378".into()),
            headshot_url: None,
        };

        replace(&store, &asset, std::slice::from_ref(&player))
            .await
            .unwrap();
        let players = all(store.reader()).await.unwrap();
        assert_eq!(players, vec![player]);
    }

    #[tokio::test]
    async fn by_gsis_finds_one_player_and_misses_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&format!("sqlite://{}/t.db", dir.path().display()))
            .await
            .unwrap();
        let asset = SyncedAsset {
            release_tag: "players",
            asset_name: "players.csv",
            updated_at: "2026-07-01T00:00:00Z",
        };
        let jordan = Player {
            gsis_id: "00-0036264".into(),
            full_name: "Jordan Love".into(),
            first_name: Some("Jordan".into()),
            last_name: Some("Love".into()),
            position: Some("QB".into()),
            latest_team: Some(TeamAbbr("GB".into())),
            espn_id: Some("4036378".into()),
            headshot_url: None,
        };
        replace(&store, &asset, std::slice::from_ref(&jordan))
            .await
            .unwrap();

        let found = by_gsis(store.reader(), "00-0036264").await.unwrap();
        assert_eq!(
            found.as_ref().map(|p| p.full_name.as_str()),
            Some("Jordan Love")
        );
        assert!(
            by_gsis(store.reader(), "00-9999999")
                .await
                .unwrap()
                .is_none(),
            "unknown id is None, not an error"
        );
    }
}
