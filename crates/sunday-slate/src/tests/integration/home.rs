//! `GET /` dispatch and shell-level home behavior, exercised through HTTP.

use axum::http::StatusCode;
use sqlx::SqlitePool;
use time::macros::datetime;

use crate::tests::TestApp;
use crate::tests::factories::{self, TeamOptions, UserOptions};

const SLATE_GAME: &str = "2025_01_BUF_KC";
const KICKOFF: time::OffsetDateTime = datetime!(2025-09-07 17:00 UTC);

#[sqlx::test]
async fn home_has_no_setup_reminders(pool: SqlitePool) {
    let app = TestApp::from_pool_at(pool.clone(), datetime!(2025-09-06 12:00 UTC)).await;
    let admin = factories::user(
        &pool,
        UserOptions {
            is_admin: true,
            ..Default::default()
        },
    )
    .await;
    factories::team(
        &pool,
        TeamOptions {
            owner: Some(admin.user.clone()),
            is_commish: true,
            ..Default::default()
        },
    )
    .await;
    factories::contest(&pool, "Week 1").await;
    app.nfl
        .seed_for_test(&[], &[factories::game_at(SLATE_GAME, 1, KICKOFF)])
        .await
        .unwrap();

    app.login(&admin).await;
    let body = app.get("/standings").await.text();
    assert!(!body.contains("Get Week 1 ready"), "{body}");
    assert!(!body.contains("Upload salaries"), "{body}");
    assert!(!body.contains("Set slate"), "{body}");
}

#[sqlx::test]
async fn home_redirects_to_the_upcoming_contest(pool: SqlitePool) {
    let app = TestApp::from_pool_at(pool.clone(), datetime!(2025-09-06 12:00 UTC)).await;
    let team = factories::team(&pool, TeamOptions::default()).await;
    let contest = factories::contest_with_games(&pool, "Week 1", &[SLATE_GAME]).await;
    let g = factories::game_at(SLATE_GAME, 1, KICKOFF);
    app.nfl.seed_for_test(&[], &[g]).await.unwrap();

    app.login_as(&team.owner).await;

    let resp = app.get("/").await;
    resp.assert_status(StatusCode::SEE_OTHER);
    assert_eq!(
        resp.header("location").to_str().unwrap(),
        format!("/contests/{contest}")
    );
    // Following the redirect lands on the contest page with the CTA.
    let body = app.home().await.text();
    assert!(body.contains("Enter lineup"), "{body}");
}

#[sqlx::test]
async fn home_redirects_to_the_live_contest(pool: SqlitePool) {
    // An hour after kickoff: the contest is live.
    let app = TestApp::from_pool_at(pool.clone(), datetime!(2025-09-07 20:00 UTC)).await;
    let team = factories::team(&pool, TeamOptions::default()).await;
    let contest = factories::contest_with_games(&pool, "Week 1", &[SLATE_GAME]).await;
    factories::full_entry(&pool, contest, team.team.id, "00-A").await;
    let g = factories::game_at(SLATE_GAME, 1, KICKOFF);
    app.nfl.seed_for_test(&[], &[g]).await.unwrap();

    app.login_as(&team.owner).await;

    let resp = app.get("/").await;
    resp.assert_status(StatusCode::SEE_OTHER);
    assert_eq!(
        resp.header("location").to_str().unwrap(),
        format!("/contests/{contest}")
    );
}

#[sqlx::test]
async fn home_stays_on_the_finished_contest_between_contests(pool: SqlitePool) {
    // Two days past Week 1's kickoff, with a Week 2 game still on the
    // schedule but no next contest published yet: the finished Week 1
    // contest is the current one (it lingers until a new contest is created).
    let app = TestApp::from_pool_at(pool.clone(), datetime!(2025-09-09 12:00 UTC)).await;
    let team = factories::team(&pool, TeamOptions::default()).await;
    let contest = factories::contest_with_games(&pool, "Week 1", &[SLATE_GAME]).await;
    let g1 = factories::game_at(SLATE_GAME, 1, KICKOFF);
    let g2 = factories::game_at("2025_02_BUF_KC", 2, datetime!(2025-09-14 17:00 UTC));
    app.nfl.seed_for_test(&[], &[g1, g2]).await.unwrap();

    app.login_as(&team.owner).await;

    let resp = app.get("/").await;
    resp.assert_status(StatusCode::SEE_OTHER);
    assert_eq!(
        resp.header("location").to_str().unwrap(),
        format!("/contests/{contest}")
    );
}

#[sqlx::test]
async fn a_holiday_week_redirects_to_its_soonest_contest(pool: SqlitePool) {
    // Thanksgiving week: two published contests share NFL week 13.
    // Current is the soonest one (Thursday); the Contests tab covers both.
    let app = TestApp::from_pool_at(pool.clone(), datetime!(2025-11-25 12:00 UTC)).await;
    let team = factories::team(&pool, TeamOptions::default()).await;
    let thu_contest =
        factories::contest_with_games(&pool, "Thanksgiving Special", &["2025_13_GB_DET"]).await;
    factories::contest_with_games(&pool, "Week 13", &["2025_13_BUF_KC"]).await;
    let thu = factories::game_at("2025_13_GB_DET", 13, datetime!(2025-11-27 18:00 UTC));
    let sun = factories::game_at("2025_13_BUF_KC", 13, datetime!(2025-11-30 18:00 UTC));
    app.nfl.seed_for_test(&[], &[thu, sun]).await.unwrap();

    app.login_as(&team.owner).await;

    let resp = app.get("/").await;
    resp.assert_status(StatusCode::SEE_OTHER);
    assert_eq!(
        resp.header("location").to_str().unwrap(),
        format!("/contests/{thu_contest}")
    );
}

#[tokio::test]
async fn a_contest_less_preseason_lands_on_standings() {
    let app = TestApp::new().await;
    let team = factories::team(&app.pool, TeamOptions::default()).await;

    app.login_as(&team.owner).await;

    let resp = app.get("/").await;
    resp.assert_status(StatusCode::SEE_OTHER);
    assert_eq!(resp.header("location").to_str().unwrap(), "/standings");
}
