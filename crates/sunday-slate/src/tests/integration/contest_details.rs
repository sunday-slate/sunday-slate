//! Contest details: route coverage for `GET /contests/{id}` through the HTTP
//! test harness.

use axum::http::StatusCode;
use sqlx::SqlitePool;

use crate::tests::TestApp;
use crate::tests::factories::{self, TeamOptions};

#[sqlx::test]
async fn contest_details_404s_unknown_and_non_numeric_ids(pool: SqlitePool) {
    let app = TestApp::from_pool(pool.clone()).await;
    let a = factories::team(&pool, TeamOptions::default()).await;
    app.login_as(&a.owner).await;
    app.get("/contests/999")
        .await
        .assert_status(StatusCode::NOT_FOUND);
    app.get("/contests/nope")
        .await
        .assert_status(StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn contest_details_requires_login(pool: SqlitePool) {
    let app = TestApp::from_pool(pool.clone()).await;
    // A user must already exist, or the app's fresh-install gate redirects
    // everything to /setup instead of exercising `login_required`.
    factories::team(&pool, TeamOptions::default()).await;
    let wk1 = factories::contest(&pool, "Week 1").await;
    let resp = app.get(&format!("/contests/{wk1}")).await;
    resp.assert_status(StatusCode::TEMPORARY_REDIRECT);
    assert!(
        resp.header("location")
            .to_str()
            .unwrap()
            .starts_with("/login"),
        "redirects to login"
    );
}
