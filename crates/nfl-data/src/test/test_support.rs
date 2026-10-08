use std::{sync::Arc, time::Duration};

use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

use crate::{NflData, NflDataConfig};

pub(crate) async fn mock_nfl(
    refresh_interval: Option<Duration>,
) -> (Arc<NflData>, MockServer, tempfile::TempDir) {
    mock_nfl_setup(refresh_interval, false, Duration::ZERO).await
}

pub(crate) async fn mock_nfl_with_schedule(
    refresh_interval: Option<Duration>,
    schedule_succeeds: bool,
) -> (Arc<NflData>, MockServer, tempfile::TempDir) {
    mock_nfl_setup(refresh_interval, schedule_succeeds, Duration::ZERO).await
}

pub(crate) async fn mock_nfl_delayed(
    delay: Duration,
) -> (Arc<NflData>, MockServer, tempfile::TempDir) {
    mock_nfl_setup(None, false, delay).await
}

async fn mock_nfl_setup(
    refresh_interval: Option<Duration>,
    schedule_succeeds: bool,
    delay: Duration,
) -> (Arc<NflData>, MockServer, tempfile::TempDir) {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    for tag in [
        "schedules",
        "players",
        "weekly_rosters",
        "stats_player",
        "pbp",
    ] {
        let response = if tag == "schedules" && schedule_succeeds {
            ResponseTemplate::new(200).set_body_string(format!(
                r#"{{"assets":[{{"name":"games.csv","browser_download_url":"{}/games.csv","updated_at":"2026-01-01T00:00:00Z"}}]}}"#,
                server.uri()
            ))
        } else {
            ResponseTemplate::new(503).set_delay(delay)
        };
        Mock::given(method("GET"))
            .and(path(format!(
                "/repos/nflverse/nflverse-data/releases/tags/{tag}"
            )))
            .respond_with(response)
            .mount(&server)
            .await;
    }
    if schedule_succeeds {
        Mock::given(method("GET"))
            .and(path("/games.csv"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                "game_id,season,week,game_type,gameday,gametime,home_team,away_team,home_score,away_score\n2025_01_BUF_KC,2025,1,REG,2025-09-07,13:00,KC,BUF,,\n",
            ))
            .mount(&server)
            .await;
    }
    let nfl = NflData::connect(NflDataConfig {
        database_url: format!("sqlite://{}/cache.db", dir.path().display()),
        github_api_base: server.uri(),
        refresh_interval,
        ..Default::default()
    })
    .await
    .unwrap();
    (Arc::new(nfl), server, dir)
}
