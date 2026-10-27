use crate::tests::TestApp;
use crate::tests::factories::{self, UserOptions};
use axum::http::StatusCode;

#[tokio::test]
async fn admin_follows_tools_to_nflverse_without_a_league_or_team() {
    let (nfl, server) = crate::tests::nfl::mocked_nfl().await;
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
async fn salary_imports_routes_require_admin_authorization() {
    let (nfl, _server) = crate::tests::nfl::mocked_nfl().await;
    let app = TestApp::new_with_nfl(nfl).await;

    // Anonymous users are redirected to login on every salary route, both verbs.
    for path in [
        "/nfl-data-admin/salary-imports",
        "/nfl-data-admin/salary-imports/2025001",
        "/nfl-data-admin/salary-imports/2025001/players/999",
        "/nfl-data-admin/salary-imports/2025001/players/999/search",
    ] {
        let resp = app.get(path).await;
        assert!(resp.status_code().is_redirection(), "{path}");
        assert!(
            resp.header("location")
                .to_str()
                .unwrap()
                .starts_with("/login"),
            "{path}"
        );
    }
    for path in [
        "/nfl-data-admin/salary-imports",
        "/nfl-data-admin/salary-imports/2025001/players/999/resolve",
    ] {
        let resp = app.post(path, "").await;
        assert!(resp.status_code().is_redirection(), "{path}");
        assert!(
            resp.header("location")
                .to_str()
                .unwrap()
                .starts_with("/login"),
            "{path}"
        );
    }

    // A signed-in non-admin gets 403 on the same routes.
    let mut app = app;
    app.clear_cookies();
    let user = factories::user(&app.pool, UserOptions::default()).await;
    app.login_as(&user.user).await;
    for path in [
        "/nfl-data-admin/salary-imports",
        "/nfl-data-admin/salary-imports/2025001",
        "/nfl-data-admin/salary-imports/2025001/players/999",
        "/nfl-data-admin/salary-imports/2025001/players/999/search",
    ] {
        app.get(path).await.assert_status(StatusCode::FORBIDDEN);
    }
    for path in [
        "/nfl-data-admin/salary-imports",
        "/nfl-data-admin/salary-imports/2025001/players/999/resolve",
    ] {
        app.post(path, "")
            .await
            .assert_status(StatusCode::FORBIDDEN);
    }

    // A global admin with no league or team can open the workflow pages.
    // (The player-resolution page 404s without an imported salary week; the
    // full workflow with data is covered by nfl-data's handler tests.)
    app.clear_cookies();
    let admin = factories::user(
        &app.pool,
        UserOptions {
            is_admin: true,
            ..Default::default()
        },
    )
    .await;
    app.login_as(&admin.user).await;
    app.get("/nfl-data-admin/salary-imports")
        .await
        .assert_status_ok();
    app.get("/nfl-data-admin/salary-imports/2025001")
        .await
        .assert_status_ok();
}

#[tokio::test]
async fn anonymous_and_non_admin_cannot_read_or_sync_nfl_data() {
    let (nfl, server) = crate::tests::nfl::mocked_nfl().await;
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
    // The old salary-imports path is gone with the module; the new one lives
    // under the nfl-data-admin mount.
    app.get("/admin/salary-imports")
        .await
        .assert_status(StatusCode::NOT_FOUND);
    let tools = app.get("/admin").await.text();
    for href in [
        "/admin/contests",
        "/nfl-data-admin/salary-imports",
        "/admin/avatars",
        "/leagues/new",
    ] {
        assert!(tools.contains(href), "{href}");
    }
}
