//! Team detail page: one fantasy team's season scores by contest.

use axum::http::StatusCode;
use sqlx::SqlitePool;

use crate::tests::factories::{self, TeamOptions};
use crate::tests::test_app::TestApp;

/// The team header is the same slim sticky band as the entry page — not a
/// boxed card with an oversized avatar.
#[sqlx::test]
async fn team_header_renders_as_sticky_band(pool: SqlitePool) {
    let app = TestApp::from_pool(pool.clone()).await;
    let t = factories::team(&pool, TeamOptions::default()).await;
    app.login_as(&t.owner).await;

    let resp = app.get(&format!("/teams/{}", t.team.id)).await;
    resp.assert_status_ok();
    let html = resp.text();
    assert!(
        html.contains("sticky top-14"),
        "header should be the sticky summary band: {html}"
    );
    assert!(
        !html.contains("size-20"),
        "the oversized avatar should shrink to match the entry band: {html}"
    );
}

#[sqlx::test]
async fn entryless_team_shows_empty_state(pool: SqlitePool) {
    let app = TestApp::from_pool(pool.clone()).await;
    let a = factories::team(&pool, TeamOptions::default()).await;
    app.login_as(&a.owner).await;
    let resp = app.get(&format!("/teams/{}", a.team.id)).await;
    resp.assert_status_ok();
    let html = resp.text();
    assert!(html.contains("No scored contests yet."), "{html}");
    assert!(!html.contains("season total"), "blank total: {html}");
}

#[sqlx::test]
async fn unknown_and_cross_league_ids_404(pool: SqlitePool) {
    let app = TestApp::from_pool(pool.clone()).await;
    // The current league resolves to the first league row, so create the
    // member's league before the foreign one.
    let a = factories::team(&pool, TeamOptions::default()).await;
    let other_league = factories::bare_league(&pool, None).await;
    let foreign = factories::team(
        &pool,
        TeamOptions {
            league: Some(other_league),
            ..Default::default()
        },
    )
    .await;

    app.login_as(&a.owner).await;
    let cross = app.get(&format!("/teams/{}", foreign.team.id)).await;
    cross.assert_status(StatusCode::NOT_FOUND);
    let unknown = app.get("/teams/999999").await;
    unknown.assert_status(StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn standings_rows_reach_team_detail(pool: SqlitePool) {
    let app = TestApp::from_pool(pool.clone()).await;
    let a = factories::team(&pool, TeamOptions::default()).await;
    app.login_as(&a.owner).await;
    let board = app.get("/standings").await;
    board.assert_status_ok();
    let href = format!(r#"href="/teams/{}""#, a.team.id);
    assert!(
        board.text().contains(&href),
        "standings links to the page: {}",
        board.text()
    );
}
