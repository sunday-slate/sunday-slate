use crate::tests::TestApp;
use crate::tests::factories::{self, UserOptions};
use axum::http::StatusCode;

#[tokio::test]
async fn admin_follows_tools_to_nflverse_without_a_league_or_team() {
    let (nfl, server, _dir) = crate::tests::nfl::mocked_nfl().await;
    let app = TestApp::new_with_nfl(nfl).await;
    let admin = factories::user(
        &app.pool,
        UserOptions {
            is_admin: true,
            ..Default::default()
        },
    )
    .await;
    app.login_as(&admin.user).await;

    let tools = app.get("/admin").await;
    tools.assert_status_ok();
    assert!(tools.text().contains("NFL Data Admin"));
    assert!(!tools.text().contains("/admin/nfl-sync"));

    let landing = app.get("/nfl-data-admin").await;
    landing.assert_status_ok();
    assert!(landing.text().contains("/nfl-data-admin/nflverse"));
    assert!(landing.text().contains("/admin"));

    let nflverse = app.get("/nfl-data-admin/nflverse").await;
    nflverse.assert_status_ok();
    assert!(nflverse.text().contains("schedules"));
    assert!(nflverse.text().contains("/nfl-data-admin"));

    let panel = app.post_htmx("/nfl-data-admin/nflverse", "").await;
    panel.assert_status_ok();
    assert!(panel.text().contains("id=\"nfl-sync-panel\""));
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while server.received_requests().await.unwrap().is_empty() {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("authorized manual refresh reaches the mocked upstream");
}

#[tokio::test]
async fn anonymous_and_non_admin_cannot_read_or_sync_nfl_data() {
    let (nfl, server, _dir) = crate::tests::nfl::mocked_nfl().await;
    let app = TestApp::new_with_nfl(nfl).await;
    for path in [
        "/nfl-data-admin",
        "/nfl-data-admin/nflverse",
        "/nfl-data-admin/static/css/admin.css",
    ] {
        let anonymous = app.get(path).await;
        assert!(anonymous.status_code().is_redirection(), "{path}");
        assert!(
            anonymous
                .header("location")
                .to_str()
                .unwrap()
                .starts_with("/login")
        );
    }
    let anonymous_post = app.post("/nfl-data-admin/nflverse", "").await;
    assert!(anonymous_post.status_code().is_redirection());
    assert!(
        anonymous_post
            .header("location")
            .to_str()
            .unwrap()
            .starts_with("/login")
    );
    let user = factories::user(&app.pool, UserOptions::default()).await;
    app.login_as(&user.user).await;
    for path in [
        "/nfl-data-admin",
        "/nfl-data-admin/nflverse",
        "/nfl-data-admin/static/css/admin.css",
    ] {
        app.get(path).await.assert_status(StatusCode::FORBIDDEN);
    }
    app.post("/nfl-data-admin/nflverse", "")
        .await
        .assert_status(StatusCode::FORBIDDEN);
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[test]
fn nfl_admin_prefix_matching_is_boundary_aware() {
    use crate::admin::is_admin_path;

    for path in [
        "/admin",
        "/admin/",
        "/admin/tools",
        "/nfl-data-admin",
        "/nfl-data-admin/nflverse",
    ] {
        assert!(is_admin_path(path), "{path}");
    }
    for path in ["/administrator", "/admin-extra", "/nfl-data-admin-extra"] {
        assert!(!is_admin_path(path), "{path}");
    }
}

#[tokio::test]
async fn old_sync_route_is_removed_and_other_tools_remain() {
    let app = TestApp::new().await;
    app.login_admin().await;
    for method in ["GET", "POST"] {
        let response = if method == "GET" {
            app.get("/admin/nfl-sync").await
        } else {
            app.post("/admin/nfl-sync", "").await
        };
        response.assert_status(StatusCode::NOT_FOUND);
    }
    let tools = app.get("/admin").await.text();
    for href in [
        "/admin/contests",
        "/admin/salary-imports",
        "/admin/avatars",
        "/leagues/new",
    ] {
        assert!(tools.contains(href), "{href}");
    }
}
