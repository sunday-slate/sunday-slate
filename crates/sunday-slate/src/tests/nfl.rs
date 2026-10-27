use std::{sync::Arc, time::Duration};

use nfl_data::{NflData, NflDataConfig};
use nflverse_data::NflverseData;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

pub(crate) async fn mocked_nfl() -> (Arc<NflData>, MockServer) {
    let server = MockServer::start().await;
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
    let provider = NflverseData::connect(nflverse_data::NflverseDataConfig {
        database_url: "sqlite::memory:".into(),
        github_api_base: server.uri(),
        ..Default::default()
    })
    .await
    .unwrap();
    let nfl = NflData::connect(
        provider,
        NflDataConfig {
            fanduel_database_url: "sqlite::memory:".into(),
            store_database_url: "sqlite::memory:".into(),
            refresh_interval: Some(Duration::ZERO),
        },
    )
    .await
    .unwrap();
    (Arc::new(nfl), server)
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
