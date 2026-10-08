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
    use crate::{NflDataConfig, test::test_support::mock_nfl_with_schedule};
    use utils::Secret;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{header, method, path},
    };

    #[tokio::test]
    async fn revision_preserves_the_existing_freshness_snapshot() {
        let (nfl, _server, _dir) = mock_nfl_with_schedule(None, true).await;
        let before = nfl.data_revision().await.unwrap();
        assert_eq!(before, nfl.data_revision().await.unwrap());
        assert!(
            nfl.provider
                .sync()
                .await
                .unwrap()
                .datasets
                .iter()
                .any(|row| {
                    row.dataset == Dataset::Schedules
                        && matches!(row.status, nflverse_data::DatasetStatus::Updated { .. })
                })
        );
        let updated = nfl.data_revision().await.unwrap();
        assert_ne!(before, updated);
        assert_eq!(updated, nfl.data_revision().await.unwrap());
    }

    #[tokio::test]
    async fn internal_sync_keeps_upstream_configuration_and_partial_failure_coverage() {
        let server = MockServer::start().await;
        let dir = tempfile::tempdir().unwrap();
        for tag in [
            "schedules",
            "players",
            "weekly_rosters",
            "stats_player",
            "pbp",
        ] {
            let response = if tag == "schedules" {
                ResponseTemplate::new(200).set_body_string(format!(
                    r#"{{"assets":[{{"name":"games.csv","browser_download_url":"{}/games.csv","updated_at":"2026-01-01T00:00:00Z"}}]}}"#,
                    server.uri()
                ))
            } else {
                ResponseTemplate::new(503)
            };
            Mock::given(method("GET"))
                .and(path(format!(
                    "/repos/nflverse/nflverse-data/releases/tags/{tag}"
                )))
                .and(header("authorization", "Bearer sentinel"))
                .respond_with(response)
                .mount(&server)
                .await;
        }
        Mock::given(method("GET"))
            .and(path("/games.csv"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                "game_id,season,week,game_type,gameday,gametime,home_team,away_team,home_score,away_score\n2025_01_BUF_KC,2025,1,REG,2025-09-07,13:00,KC,BUF,,\n2024_01_BUF_KC,2024,1,REG,2024-09-08,13:00,KC,BUF,,\n",
            ))
            .mount(&server)
            .await;
        let nfl = crate::NflData::connect(NflDataConfig {
            database_url: format!("sqlite://{}/cache.db", dir.path().display()),
            fanduel_database_url: format!("sqlite://{}/fanduel.db", dir.path().display()),
            earliest_season: 2025,
            github_token: Some(Secret::new("sentinel")),
            github_api_base: server.uri(),
            refresh_interval: None,
        })
        .await
        .unwrap();
        let report = nfl.provider.sync().await.unwrap();
        assert!(!report.all_ok());
        assert!(matches!(
            report.datasets[0].status,
            nflverse_data::DatasetStatus::Updated { assets: 1, rows: 1 }
        ));
        assert!(
            report.datasets[1..]
                .iter()
                .all(|row| matches!(row.status, nflverse_data::DatasetStatus::Failed(_)))
        );
        assert_eq!(nfl.games(crate::Season(2025)).await.unwrap().len(), 1);
        assert!(nfl.games(crate::Season(2024)).await.unwrap().is_empty());
        assert!(nfl.data_revision().await.is_ok());
    }

    #[tokio::test]
    async fn revision_survives_reopening_and_seeding_preserves_bookkeeping() {
        let dir = tempfile::tempdir().unwrap();
        let config = NflDataConfig {
            database_url: format!("sqlite://{}/cache.db", dir.path().display()),
            fanduel_database_url: format!("sqlite://{}/fanduel.db", dir.path().display()),
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
