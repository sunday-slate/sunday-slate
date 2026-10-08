use nflverse_data::Dataset;
use time::OffsetDateTime;

/// Opaque snapshot hint used to detect changes in NFL-data freshness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataRevision(Vec<(Dataset, u32, Option<OffsetDateTime>)>);

impl DataRevision {
    pub(crate) fn from_snapshot(snapshot: Vec<(Dataset, u32, Option<OffsetDateTime>)>) -> Self {
        Self(snapshot)
    }
}

#[cfg(test)]
mod tests {
    use super::Dataset;
    use crate::{NflDataConfig, test_support::mock_nfl_with_schedule};

    #[tokio::test]
    async fn revision_preserves_the_existing_freshness_snapshot() {
        let (nfl, _server, _dir) = mock_nfl_with_schedule(None, true).await;
        let before = nfl.data_revision().await.unwrap();
        assert_eq!(before, nfl.data_revision().await.unwrap());
        assert!(nfl.sync().await.unwrap().datasets.iter().any(|row| {
            row.dataset == Dataset::Schedules
                && matches!(row.status, nflverse_data::DatasetStatus::Updated { .. })
        }));
        let updated = nfl.data_revision().await.unwrap();
        assert_ne!(before, updated);
        assert_eq!(updated, nfl.data_revision().await.unwrap());
    }

    #[tokio::test]
    async fn revision_survives_reopening_and_seeding_preserves_bookkeeping() {
        let dir = tempfile::tempdir().unwrap();
        let config = NflDataConfig {
            database_url: format!("sqlite://{}/cache.db", dir.path().display()),
            refresh_interval: None,
            ..Default::default()
        };
        let nfl = crate::NflData::connect(config.clone()).await.unwrap();
        let initial = nfl.data_revision().await.unwrap();
        let player = crate::Player {
            gsis_id: "00-seed".into(),
            espn_id: None,
            full_name: "Seeded".into(),
            first_name: None,
            last_name: None,
            position: None,
            latest_team: None,
            headshot_url: None,
        };
        nfl.seed_for_test(&[player], &[]).await.unwrap();
        assert_eq!(initial, nfl.data_revision().await.unwrap());
        drop(nfl);
        let reopened = crate::NflData::connect(config).await.unwrap();
        assert_eq!(initial, reopened.data_revision().await.unwrap());
    }

    #[tokio::test]
    async fn revision_read_failure_is_an_error() {
        let (nfl, _server, dir) = mock_nfl_with_schedule(None, false).await;
        let database_url = format!("sqlite://{}/cache.db", dir.path().display());
        let db = sqlx::SqlitePool::connect(&database_url).await.unwrap();
        sqlx::query("DROP TABLE sync_state")
            .execute(&db)
            .await
            .unwrap();
        db.close().await;
        assert!(nfl.data_revision().await.is_err());
    }
}
