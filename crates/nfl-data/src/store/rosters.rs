use sqlx::SqlitePool;

use crate::error::NflDataError;
use crate::model::{RosterEntry, Season, TeamAbbr};
use crate::store::Store;
use crate::store::sync_state::{self, SyncedAsset};

pub(crate) async fn replace(
    store: &Store,
    asset: &SyncedAsset<'_>,
    season: Season,
    entries: &[RosterEntry],
) -> Result<u64, NflDataError> {
    for e in entries {
        debug_assert_eq!(
            e.season, season,
            "roster entry season must match the replace() season param"
        );
    }

    store
        .write_tx(async |conn| {
            sqlx::query!("DELETE FROM roster_entries WHERE season = ?", season)
                .execute(&mut *conn)
                .await?;
            for e in entries {
                sqlx::query!(
                    r#"INSERT INTO roster_entries
                       (season, team, gsis_id, full_name, position, jersey_number, status)
                       VALUES (?, ?, ?, ?, ?, ?, ?)"#,
                    e.season,
                    e.team,
                    e.gsis_id,
                    e.full_name,
                    e.position,
                    e.jersey_number,
                    e.status
                )
                .execute(&mut *conn)
                .await?;
            }
            sync_state::record(conn, asset).await?;
            Ok(entries.len() as u64)
        })
        .await
}

pub(crate) async fn for_team(
    pool: &SqlitePool,
    season: Season,
    team: &TeamAbbr,
) -> Result<Vec<RosterEntry>, NflDataError> {
    Ok(sqlx::query_as!(
        RosterEntry,
        r#"SELECT
               season AS "season: Season",
               team AS "team: TeamAbbr",
               gsis_id,
               full_name,
               position,
               jersey_number AS "jersey_number: u16",
               status
           FROM roster_entries
           WHERE season = ? AND team = ?
           ORDER BY full_name, id"#,
        season,
        team
    )
    .fetch_all(pool)
    .await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, gsis: Option<&str>) -> RosterEntry {
        RosterEntry {
            season: Season(2025),
            team: TeamAbbr("GB".into()),
            gsis_id: gsis.map(String::from),
            full_name: name.into(),
            position: Some("QB".into()),
            jersey_number: Some(10),
            status: "ACT".into(),
        }
    }

    #[tokio::test]
    async fn replace_scopes_deletes_to_the_season() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&format!("sqlite://{}/t.db", dir.path().display()))
            .await
            .unwrap();
        let asset_2025 = SyncedAsset {
            release_tag: "rosters",
            asset_name: "roster_2025.csv",
            updated_at: "2026-06-20T00:00:00Z",
        };
        let asset_2024 = SyncedAsset {
            release_tag: "rosters",
            asset_name: "roster_2024.csv",
            updated_at: "2025-03-01T00:00:00Z",
        };

        let mut entry_2024 = entry("Jordan Love", Some("00-0036264"));
        entry_2024.season = Season(2024);
        replace(&store, &asset_2024, Season(2024), &[entry_2024.clone()])
            .await
            .unwrap();
        replace(
            &store,
            &asset_2025,
            Season(2025),
            &[
                entry("Jordan Love", Some("00-0036264")),
                entry("Dante Barnett", None),
            ],
        )
        .await
        .unwrap();

        // Re-replacing 2025 must not touch 2024.
        replace(
            &store,
            &asset_2025,
            Season(2025),
            &[entry("Jordan Love", Some("00-0036264"))],
        )
        .await
        .unwrap();

        let gb_2025 = for_team(store.reader(), Season(2025), &TeamAbbr("GB".into()))
            .await
            .unwrap();
        assert_eq!(gb_2025.len(), 1);
        let gb_2024 = for_team(store.reader(), Season(2024), &TeamAbbr("GB".into()))
            .await
            .unwrap();
        assert_eq!(gb_2024, vec![entry_2024]);
    }
}
