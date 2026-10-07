use std::{sync::Arc, time::Duration};

use nfl_data::{NflData, NflDataConfig};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

pub(crate) async fn mocked_nfl() -> (Arc<NflData>, MockServer, tempfile::TempDir) {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    for tag in [
        "schedules",
        "players",
        "weekly_rosters",
        "stats_player",
        "pbp",
    ] {
        Mock::given(method("GET"))
            .and(path(format!(
                "/repos/nflverse/nflverse-data/releases/tags/{tag}"
            )))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;
    }
    let nfl = NflData::connect(NflDataConfig {
        database_url: format!("sqlite://{}/cache.db", dir.path().display()),
        fanduel_database_url: format!("sqlite://{}/fanduel.db", dir.path().display()),
        github_api_base: server.uri(),
        refresh_interval: Some(Duration::ZERO),
        ..Default::default()
    })
    .await
    .unwrap();
    (Arc::new(nfl), server, dir)
}

pub(crate) async fn mount_schedule_release(server: &MockServer, updated_at: &str, csv: &str) {
    server.reset().await;
    for tag in ["players", "weekly_rosters", "stats_player", "pbp"] {
        Mock::given(method("GET"))
            .and(path(format!(
                "/repos/nflverse/nflverse-data/releases/tags/{tag}"
            )))
            .respond_with(ResponseTemplate::new(503))
            .mount(server)
            .await;
    }
    Mock::given(method("GET"))
        .and(path("/repos/nflverse/nflverse-data/releases/tags/schedules"))
        .respond_with(ResponseTemplate::new(200).set_body_string(format!(
            r#"{{"assets":[{{"name":"games.csv","browser_download_url":"{}/games.csv","updated_at":"{}"}}]}}"#,
            server.uri(), updated_at
        )))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/games.csv"))
        .respond_with(ResponseTemplate::new(200).set_body_string(csv.to_owned()))
        .mount(server)
        .await;
}
