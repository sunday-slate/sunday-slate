use std::{sync::Arc, time::Duration};

use nfl_data::{NflData, NflDataConfig};
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
    let nfl = NflData::connect(NflDataConfig {
        nflverse_database_url: "sqlite::memory:".into(),
        fanduel_database_url: "sqlite::memory:".into(),
        database_url: "sqlite::memory:".into(),
        github_api_base: server.uri(),
        refresh_interval: Some(Duration::ZERO),
        ..Default::default()
    })
    .await
    .unwrap();
    (Arc::new(nfl), server)
}
