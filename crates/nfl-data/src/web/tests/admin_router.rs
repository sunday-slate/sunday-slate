use std::{sync::Arc, time::Duration};

use axum::{Router, http::StatusCode};
use axum_test::TestServer;
use nfl_data::{NflData, NflDataConfig, admin_router};
use sqlx::Connection;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

async fn server() -> (TestServer, MockServer) {
    let upstream = MockServer::start().await;
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
            .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"assets":[]}"#))
            .mount(&upstream)
            .await;
    }
    let nfl = NflData::connect(NflDataConfig {
        database_url: "sqlite::memory:".into(),
        github_api_base: upstream.uri(),
        refresh_interval: None,
        ..Default::default()
    })
    .await
    .unwrap();
    (
        TestServer::new(Router::new().nest("/nfl-data-admin", admin_router::<()>(Arc::new(nfl)))),
        upstream,
    )
}

#[tokio::test]
async fn nflverse_idle_full_page_and_htmx_panel() {
    let (server, _) = server().await;
    let page = server.get("/nfl-data-admin/nflverse").await;
    assert_eq!(page.status_code(), StatusCode::OK);
    assert!(page.text().contains("<!doctype html>"));
    assert_eq!(page.text().matches("Never synced").count(), 5);
    for name in [
        "schedules",
        "players",
        "weekly_rosters",
        "player_week_stats",
        "team_week_stats",
    ] {
        assert!(page.text().contains(&format!("<td>{name}</td>")), "{name}");
    }

    let panel = server
        .get("/nfl-data-admin/nflverse")
        .add_header("HX-Request", "true")
        .await;
    assert_eq!(panel.status_code(), StatusCode::OK);
    assert!(panel.text().contains("id=\"nfl-sync-panel\""));
    assert!(!panel.text().contains("<!doctype html>"));
}

#[tokio::test]
async fn nflverse_setup_errors_are_generic_500_responses() {
    let upstream = MockServer::start().await;
    let cache = tempfile::tempdir().unwrap();
    let database_url = format!("sqlite://{}/cache.db", cache.path().display());
    let nfl = NflData::connect(NflDataConfig {
        database_url: database_url.clone(),
        github_api_base: upstream.uri(),
        refresh_interval: None,
        ..Default::default()
    })
    .await
    .unwrap();
    let mut connection = sqlx::SqliteConnection::connect(&database_url)
        .await
        .unwrap();
    sqlx::query("DROP TABLE sync_state")
        .execute(&mut connection)
        .await
        .unwrap();
    let server =
        TestServer::new(Router::new().nest("/nfl-data-admin", admin_router::<()>(Arc::new(nfl))));
    for response in [
        server.get("/nfl-data-admin/nflverse").await,
        server.post("/nfl-data-admin/nflverse").await,
    ] {
        assert_eq!(response.status_code(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(response.text(), "Internal server error");
        assert!(!response.text().contains(cache.path().to_str().unwrap()));
    }
}

#[tokio::test]
async fn nflverse_post_and_scheduled_refresh_are_single_flight() {
    let upstream = MockServer::start().await;
    let cache = tempfile::tempdir().unwrap();
    let delay = Duration::from_millis(350);
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
            .respond_with(
                ResponseTemplate::new(503)
                    .set_body_string("temporary mock failure")
                    .set_delay(delay),
            )
            .mount(&upstream)
            .await;
    }
    let interval = Duration::from_millis(60);
    let nfl = Arc::new(
        NflData::connect(NflDataConfig {
            database_url: format!("sqlite://{}/cache.db", cache.path().display()),
            github_api_base: upstream.uri(),
            refresh_interval: Some(interval),
            ..Default::default()
        })
        .await
        .unwrap(),
    );
    let server = TestServer::new(
        Router::new().nest("/nfl-data-admin", admin_router::<()>(Arc::clone(&nfl))),
    );
    nfl.start_background_tasks();

    let started = server.post("/nfl-data-admin/nflverse").await;
    assert_eq!(started.status_code(), StatusCode::OK);
    assert!(started.text().contains("Sync in progress"));
    let mut active_run_observed = false;
    for _ in 0..100 {
        if !upstream.received_requests().await.unwrap().is_empty() {
            active_run_observed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        active_run_observed,
        "manual refresh reached a delayed release"
    );
    // Leave enough time for the configured timer tick to fire while the
    // delayed manual refresh remains active.
    tokio::time::sleep(interval * 2).await;

    let (full_response, panel_response) = tokio::join!(
        server.post("/nfl-data-admin/nflverse"),
        server
            .post("/nfl-data-admin/nflverse")
            .add_header("HX-Request", "true"),
    );
    assert_eq!(full_response.status_code(), StatusCode::OK);
    assert!(full_response.text().contains("<!doctype html>"));
    assert!(full_response.text().contains("Sync in progress"));
    assert_eq!(panel_response.status_code(), StatusCode::OK);
    assert!(panel_response.text().contains("id=\"nfl-sync-panel\""));
    assert!(panel_response.text().contains("Sync in progress"));
    assert!(!panel_response.text().contains("<!doctype html>"));

    // The configured timer ticks while the detached manual run is active.
    // Stop future ticks before that run completes, then verify no overlap.
    nfl.stop_background_tasks();
    let mut completed = false;
    for _ in 0..160 {
        let status = server.get("/nfl-data-admin/nflverse").await;
        if !status.text().contains("Sync in progress") {
            completed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(completed, "delayed scheduled refresh completed");

    let requests = upstream.received_requests().await.unwrap();
    for tag in [
        "schedules",
        "players",
        "weekly_rosters",
        "stats_player",
        "pbp",
    ] {
        let endpoint = format!("/repos/nflverse/nflverse-data/releases/tags/{tag}");
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.url.path() == endpoint)
                .count(),
            1,
            "one active-run request for {tag}"
        );
    }
}

#[tokio::test]
async fn nflverse_post_renders_page_and_htmx_panel() {
    let (server, upstream) = server().await;
    for htmx in [false, true] {
        let mut request = server.post("/nfl-data-admin/nflverse");
        if htmx {
            request = request.add_header("HX-Request", "true");
        }
        let response = request.await;
        assert_eq!(response.status_code(), StatusCode::OK);
        if htmx {
            assert!(response.text().contains("id=\"nfl-sync-panel\""));
            assert!(!response.text().contains("<!doctype html>"));
        } else {
            assert!(response.text().contains("<!doctype html>"));
        }
    }
    for _ in 0..100 {
        if upstream.received_requests().await.unwrap().len() >= 5 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(!upstream.received_requests().await.unwrap().is_empty());
}
