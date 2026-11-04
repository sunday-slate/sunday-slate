use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use wiremock::{
    Mock, MockServer, Request, ResponseTemplate,
    matchers::{method, path, path_regex},
};

use crate::{NflData, NflDataConfig};

pub(crate) async fn mock_nfl(refresh_interval: Option<Duration>) -> (Arc<NflData>, MockServer) {
    mock_nfl_setup(refresh_interval, false, Duration::ZERO).await
}

pub(crate) async fn mock_nfl_with_schedule(
    refresh_interval: Option<Duration>,
    schedule_succeeds: bool,
) -> (Arc<NflData>, MockServer) {
    mock_nfl_setup(refresh_interval, schedule_succeeds, Duration::ZERO).await
}

pub(crate) async fn mock_nfl_delayed(delay: Duration) -> (Arc<NflData>, MockServer) {
    mock_nfl_setup(None, false, delay).await
}

/// One asset the mock's release listing advertises and its download serves.
#[derive(Clone)]
pub(crate) struct MockAsset {
    pub(crate) name: String,
    pub(crate) updated_at: String,
    pub(crate) body: String,
    pub(crate) status: u16,
}

impl MockAsset {
    /// A downloadable asset.
    pub(crate) fn csv(name: &str, updated_at: &str, body: impl Into<String>) -> Self {
        Self {
            name: name.to_owned(),
            updated_at: updated_at.to_owned(),
            body: body.into(),
            status: 200,
        }
    }

    /// An asset whose listing the sync sees but whose download fails.
    pub(crate) fn failing(name: &str, updated_at: &str, status: u16) -> Self {
        Self {
            name: name.to_owned(),
            updated_at: updated_at.to_owned(),
            body: String::new(),
            status,
        }
    }
}

/// A wiremock server shaped like the nflverse releases API: five release
/// listings whose asset sets tests replace between syncs. A tag without a
/// listing serves no assets.
pub(crate) struct NflverseMock {
    pub(crate) server: MockServer,
    /// Asset listings per release tag, replaced by tests between syncs.
    releases: Arc<Mutex<BTreeMap<String, Vec<MockAsset>>>>,
}

impl NflverseMock {
    pub(crate) async fn start() -> Self {
        let server = MockServer::start().await;
        let releases: Arc<Mutex<BTreeMap<String, Vec<MockAsset>>>> = Arc::default();
        let base = server.uri();

        // Asset downloads are any top-level path; release listings live under
        // /repos/…, whose separators never match.
        {
            let releases = Arc::clone(&releases);
            Mock::given(method("GET"))
                .and(path_regex("^/[a-zA-Z0-9_.]+$"))
                .respond_with(move |request: &Request| {
                    let name = request.url.path().trim_start_matches('/');
                    let releases = releases.lock().unwrap_or_else(|error| error.into_inner());
                    match releases
                        .values()
                        .flatten()
                        .find(|asset| asset.name == name)
                        .cloned()
                    {
                        Some(asset) => {
                            ResponseTemplate::new(asset.status).set_body_string(asset.body)
                        }
                        None => ResponseTemplate::new(404),
                    }
                })
                .mount(&server)
                .await;
        }
        for tag in [
            "schedules",
            "players",
            "weekly_rosters",
            "stats_player",
            "pbp",
        ] {
            let releases = Arc::clone(&releases);
            let base = base.clone();
            Mock::given(method("GET"))
                .and(path(format!(
                    "/repos/nflverse/nflverse-data/releases/tags/{tag}"
                )))
                .respond_with(move |_request: &Request| {
                    let releases = releases.lock().unwrap_or_else(|error| error.into_inner());
                    let empty: Vec<MockAsset> = Vec::new();
                    let assets = releases.get(tag).unwrap_or(&empty);
                    // Hand-built JSON: every value is fixture-controlled ASCII.
                    let items: Vec<String> = assets
                        .iter()
                        .map(|asset| {
                            format!(
                                "{{\"name\":\"{}\",\"updated_at\":\"{}\",\"browser_download_url\":\"{}/{}\"}}",
                                asset.name, asset.updated_at, base, asset.name
                            )
                        })
                        .collect();
                    ResponseTemplate::new(200)
                        .set_body_string(format!("{{\"assets\":[{}]}}", items.join(",")))
                })
                .mount(&server)
                .await;
        }
        Self { server, releases }
    }

    /// Replace a release tag's asset listing.
    pub(crate) fn set_release(&self, tag: &str, assets: Vec<MockAsset>) {
        self.releases
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(tag.to_owned(), assets);
    }

    pub(crate) fn uri(&self) -> String {
        self.server.uri()
    }
}

async fn mock_nfl_setup(
    refresh_interval: Option<Duration>,
    schedule_succeeds: bool,
    delay: Duration,
) -> (Arc<NflData>, MockServer) {
    let server = MockServer::start().await;
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
        nflverse_database_url: "sqlite::memory:".into(),
        fanduel_database_url: "sqlite::memory:".into(),
        database_url: "sqlite::memory:".into(),
        github_api_base: server.uri(),
        refresh_interval,
        ..Default::default()
    })
    .await
    .unwrap();
    (Arc::new(nfl), server)
}
