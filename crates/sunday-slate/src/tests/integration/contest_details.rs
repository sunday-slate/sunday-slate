//! Contest details: route coverage for `GET /contests/{id}` through the HTTP
//! test harness.

use axum::http::StatusCode;
use nfl_data::{Game, PlayerWeekStats, Season, TeamAbbr, TeamWeekStats};
use sqlx::SqlitePool;
use time::macros::datetime;

use crate::tests::TestApp;
use crate::tests::factories::{self, TeamOptions};
use crate::tests::utils::list_item_containing;

#[sqlx::test]
async fn contest_details_ranks_entries_and_crowns_the_winner(pool: SqlitePool) {
    // Two days past the slate's only kickoff: the contest has resolved.
    let app = TestApp::from_pool_at(pool.clone(), datetime!(2025-09-09 17:00 UTC)).await;
    let a = factories::team(
        &pool,
        TeamOptions {
            name: Some("Gridiron Giants".into()),
            owner_name: Some("Mike".into()),
            ..Default::default()
        },
    )
    .await;
    let b = factories::team(
        &pool,
        TeamOptions {
            league: Some(a.league.clone()),
            name: Some("Turf Titans".into()),
            owner_name: Some("Sara".into()),
            ..Default::default()
        },
    )
    .await;
    let wk1 = factories::contest(&pool, "Week 1").await;
    sqlx::query(
        "INSERT INTO contest_games (contest_id, gsis_game_id) VALUES (?1, '2025_01_BUF_KC')",
    )
    .bind(wk1)
    .execute(&pool)
    .await
    .unwrap();
    let giants_entry = factories::full_entry(&pool, wk1, a.team.id, "00-A").await; // 100yd = 13.00
    let titans_entry = factories::full_entry(&pool, wk1, b.team.id, "00-B").await; // 200yd = 23.00
    let game = Game {
        kickoff: Some(datetime!(2025-09-07 17:00 UTC)),
        ..factories::game("2025_01_BUF_KC", 1)
    };
    app.nfl.seed_for_test(&[], &[game]).await.unwrap();
    let buf_player = PlayerWeekStats {
        gsis_id: "00-BUF".into(),
        team: TeamAbbr("BUF".into()),
        opponent: Some(TeamAbbr("KC".into())),
        ..factories::player_stats("00-BUF", 1)
    };
    let buf_defense = TeamWeekStats {
        gsis_game_id: "2025_01_BUF_KC".into(),
        ..factories::scoreless_defense_stats("BUF", "KC", 1)
    };
    app.nfl
        .seed_week_stats_for_test(
            Season(2025),
            &[
                PlayerWeekStats {
                    opponent: Some(TeamAbbr("BUF".into())),
                    ..factories::rb_stats("00-A", 1, 100)
                },
                PlayerWeekStats {
                    opponent: Some(TeamAbbr("BUF".into())),
                    ..factories::rb_stats("00-B", 1, 200)
                },
                buf_player,
            ],
            &[
                factories::scoreless_defense_stats("KC", "BUF", 1),
                buf_defense,
            ],
        )
        .await
        .unwrap();

    app.login_as(&a.owner).await;
    let resp = app.get(&format!("/contests/{wk1}")).await;
    resp.assert_status_ok();
    let html = resp.text();
    assert!(html.contains("Week 1"));
    let titans = html.find("Turf Titans").expect("titans present");
    let giants = html.find("Gridiron Giants").expect("giants present");
    assert!(titans < giants, "winner (23.00) ranked above 18... (13.00)");
    assert!(html.contains("23.00"));
    assert!(html.contains("13.00"));
    let content = main_content(&html);
    let titans_item = list_item_containing(content, &format!(r#"href="/entries/{titans_entry}""#));
    let giants_item = list_item_containing(content, &format!(r#"href="/entries/{giants_entry}""#));
    assert!(
        content.contains("ph-fill ph-trophy"),
        "winner treatment shown"
    );
    assert!(
        giants_item.contains("bg-primary/10"),
        "viewer's losing entry is highlighted"
    );
    assert!(
        giants_item.contains("hover:bg-primary/20"),
        "viewer's losing entry strengthens on hover"
    );
    assert!(
        !titans_item.contains("bg-primary/10"),
        "winner is not highlighted by rank"
    );
    assert!(
        !titans_item.contains("hover:bg-primary/20"),
        "winner keeps ordinary hover"
    );
    assert!(
        titans_item.contains("hover:bg-base-200"),
        "winner keeps ordinary interaction"
    );
    assert!(!html.contains("BUF @ KC"), "resolved slate hides its games");
}

/// The `<main>` region, excluding the nav drawer — whose active Contests
/// entry also carries `ph-fill ph-trophy`, so a body-wide search for the
/// winner's trophy icon would false-positive on it.
fn main_content(html: &str) -> &str {
    html.split_once("<main")
        .map(|(_, rest)| rest)
        .expect("main renders")
        .split_once("</main>")
        .map(|(before, _)| before)
        .expect("main closes")
}

#[sqlx::test]
async fn contest_details_pre_results_hides_points(pool: SqlitePool) {
    let app = TestApp::from_pool(pool.clone()).await;
    let a = factories::team(&pool, TeamOptions::default()).await;
    let wk1 = factories::contest(&pool, "Week 1").await;
    sqlx::query(
        "INSERT INTO contest_games (contest_id, gsis_game_id) VALUES (?1, '2025_01_BUF_KC')",
    )
    .bind(wk1)
    .execute(&pool)
    .await
    .unwrap();
    factories::full_entry(&pool, wk1, a.team.id, "00-A").await;
    // Week resolves, stats exist, but the entry's players have no stat
    // lines -> everyone scores 0.0 -> the results gate stays closed.
    app.nfl
        .seed_for_test(&[], &[factories::game("2025_01_BUF_KC", 1)])
        .await
        .unwrap();
    app.nfl
        .seed_week_stats_for_test(
            Season(2025),
            &[factories::rb_stats("00-Z", 1, 100)],
            &[factories::scoreless_defense_stats("KC", "BUF", 1)],
        )
        .await
        .unwrap();

    app.login_as(&a.owner).await;
    let resp = app.get(&format!("/contests/{wk1}")).await;
    resp.assert_status_ok();
    let html = resp.text();
    assert!(!html.contains("0.00"), "no zero points pre-results: {html}");
    let content = main_content(&html);
    assert!(
        !content.contains("ph-fill ph-trophy"),
        "no winner pre-results: {html}"
    );
}

#[sqlx::test]
async fn contest_details_lists_teams_still_to_enter(pool: SqlitePool) {
    // Three calendar dates before kickoff, but after Thursday's kickoff hour.
    let app = TestApp::from_pool_at(pool.clone(), datetime!(2025-09-04 20:00 UTC)).await;
    let league = factories::bare_league(&pool, Some("Sunday Funday".into())).await;
    let entered = factories::team(
        &pool,
        TeamOptions {
            league: Some(league.clone()),
            name: Some("Gridiron Giants".into()),
            owner_name: Some("Mike".into()),
            ..Default::default()
        },
    )
    .await;
    let waiting = factories::team(
        &pool,
        TeamOptions {
            league: Some(league.clone()),
            name: Some("Turf Titans".into()),
            owner_name: Some("Sara".into()),
            ..Default::default()
        },
    )
    .await;
    let wk1 = factories::contest(&pool, "Week 1").await;
    sqlx::query(
        "INSERT INTO contest_games (contest_id, gsis_game_id) VALUES (?1, '2025_01_BUF_KC')",
    )
    .bind(wk1)
    .execute(&pool)
    .await
    .unwrap();
    factories::full_entry(&pool, wk1, entered.team.id, "00-A").await;
    let kicking_off = Game {
        kickoff: Some(datetime!(2025-09-07 17:00 UTC)),
        ..factories::game("2025_01_BUF_KC", 1)
    };
    app.nfl.seed_for_test(&[], &[kicking_off]).await.unwrap();

    app.login_as(&waiting.owner).await;
    let resp = app.get(&format!("/contests/{wk1}")).await;
    resp.assert_status_ok();
    let html = resp.text();
    assert!(
        html.contains("Kicks off Sun, Sep 7 · 1:00 PM ET"),
        "kickoff header: {html}"
    );
    assert!(html.contains("in 3 days"), "calendar-day countdown: {html}");
    assert!(html.contains("Entries · 1"), "{html}");
    assert!(html.contains("Still to enter · 1"), "{html}");

    let row = list_item_containing(&html, "Turf Titans");
    assert!(
        row.contains(&format!("/contests/{wk1}/draft-entry")),
        "the viewer's own waiting row links to the editor: {row}"
    );
    assert!(row.contains("bg-primary/10"), "and is highlighted: {row}");
    assert!(
        !list_item_containing(&html, "Gridiron Giants").contains("draft-entry"),
        "an entered team is not offered the editor: {html}"
    );
}
#[sqlx::test]
async fn contest_details_normalizes_dst_transition_for_kickoff_and_countdown(pool: SqlitePool) {
    let app = TestApp::from_pool_at(pool.clone(), datetime!(2025-11-02 04:30 UTC)).await;
    let member = factories::team(&pool, TeamOptions::default()).await;
    let wk10 = factories::contest(&pool, "Week 10").await;
    sqlx::query(
        "INSERT INTO contest_games (contest_id, gsis_game_id) VALUES (?1, '2025_10_BUF_KC')",
    )
    .bind(wk10)
    .execute(&pool)
    .await
    .unwrap();
    let kicking_off = Game {
        kickoff: Some(datetime!(2025-11-06 18:00 UTC)),
        ..factories::game("2025_10_BUF_KC", 10)
    };
    app.nfl.seed_for_test(&[], &[kicking_off]).await.unwrap();

    app.login_as(&member.owner).await;
    let html = app.get(&format!("/contests/{wk10}")).await.text();
    assert!(
        html.contains("Kicks off Thu, Nov 6 · 1:00 PM ET"),
        "kickoff header: {html}"
    );
    assert!(
        html.contains("Entries lock at kickoff — in 4 days"),
        "countdown: {html}"
    );
}

#[sqlx::test]
async fn contest_details_omits_the_waiting_list_when_everyone_entered(pool: SqlitePool) {
    let app = TestApp::from_pool_at(pool.clone(), datetime!(2025-09-06 17:00 UTC)).await;
    let league = factories::bare_league(&pool, Some("Sunday Funday".into())).await;
    let only = factories::team(
        &pool,
        TeamOptions {
            league: Some(league.clone()),
            name: Some("Gridiron Giants".into()),
            ..Default::default()
        },
    )
    .await;
    let wk1 = factories::contest(&pool, "Week 1").await;
    sqlx::query(
        "INSERT INTO contest_games (contest_id, gsis_game_id) VALUES (?1, '2025_01_BUF_KC')",
    )
    .bind(wk1)
    .execute(&pool)
    .await
    .unwrap();
    factories::full_entry(&pool, wk1, only.team.id, "00-A").await;
    let kicking_off = Game {
        kickoff: Some(datetime!(2025-09-07 17:00 UTC)),
        ..factories::game("2025_01_BUF_KC", 1)
    };
    app.nfl.seed_for_test(&[], &[kicking_off]).await.unwrap();

    app.login_as(&only.owner).await;
    let html = app.get(&format!("/contests/{wk1}")).await.text();
    assert!(html.contains("Entries · 1"), "{html}");
    assert!(!html.contains("Still to enter"), "{html}");
}

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

#[sqlx::test]
async fn an_unpublished_contest_shows_no_games(pool: SqlitePool) {
    // Before kickoff, so the contest reads as upcoming at both the old and
    // new code — the point where the two paths can actually diverge.
    let app = TestApp::from_pool_at(pool.clone(), datetime!(2025-09-01 00:00 UTC)).await;
    let a = factories::team(&pool, TeamOptions::default()).await;
    // A contest with a rule and a schedule behind it, but no published slate.
    let wk1 = factories::contest(&pool, "Week 1").await;
    // Sunday 1:00 PM ET kickoff, so the old formula would have proposed it.
    let kicking_off = Game {
        kickoff: Some(datetime!(2025-09-07 17:00 UTC)),
        ..factories::game("2025_01_BUF_KC", 1)
    };
    app.nfl.seed_for_test(&[], &[kicking_off]).await.unwrap();

    app.login_as(&a.owner).await;
    let resp = app.get(&format!("/contests/{wk1}")).await;
    resp.assert_status_ok();
    assert!(
        !resp.text().contains("BUF @ KC"),
        "the formula's guess must not reach the page"
    );
}

#[sqlx::test]
async fn contest_details_rows_link_to_their_entries(pool: SqlitePool) {
    let app = TestApp::from_pool(pool.clone()).await;
    let a = factories::team(&pool, TeamOptions::default()).await;
    let wk1 = factories::contest(&pool, "Week 1").await;
    sqlx::query(
        "INSERT INTO contest_games (contest_id, gsis_game_id) VALUES (?1, '2025_01_BUF_KC')",
    )
    .bind(wk1)
    .execute(&pool)
    .await
    .unwrap();
    let entry = factories::full_entry(&pool, wk1, a.team.id, "00-A").await;
    app.nfl
        .seed_for_test(&[], &[factories::game("2025_01_BUF_KC", 1)])
        .await
        .unwrap();
    app.nfl
        .seed_week_stats_for_test(
            Season(2025),
            &[factories::rb_stats("00-A", 1, 100)],
            &[factories::scoreless_defense_stats("KC", "BUF", 1)],
        )
        .await
        .unwrap();

    app.login_as(&a.owner).await;
    let html = app.get(&format!("/contests/{wk1}")).await.text();
    assert!(
        html.contains(&format!("href=\"/entries/{entry}\"")),
        "the entry row links to its entry: {html}"
    );
    // The link is not a dead end: the target renders.
    app.get(&format!("/entries/{entry}"))
        .await
        .assert_status_ok();
}

#[sqlx::test]
async fn contest_page_resumes_a_started_draft(pool: SqlitePool) {
    let app = TestApp::from_pool_at(pool.clone(), datetime!(2025-09-06 12:00 UTC)).await;
    let team = factories::team(&pool, TeamOptions::default()).await;
    let contest = factories::contest_with_games(&pool, "Week 1", &["2025_01_BUF_KC"]).await;
    let g = factories::game_at("2025_01_BUF_KC", 1, datetime!(2025-09-07 17:00 UTC));
    app.nfl.seed_for_test(&[], &[g]).await.unwrap();
    sqlx::query(
        "INSERT INTO draft_entries (contest_id, fantasy_team_id, qb_gsis_id) VALUES (?1, ?2, '00-A')",
    )
    .bind(contest)
    .bind(team.team.id)
    .execute(&pool)
    .await
    .unwrap();

    app.login_as(&team.owner).await;
    let body = app.get(&format!("/contests/{contest}")).await.text();
    assert!(body.contains("Resume lineup"), "{body}");
}

#[sqlx::test]
async fn contest_page_omits_cta_once_entered(pool: SqlitePool) {
    let app = TestApp::from_pool_at(pool.clone(), datetime!(2025-09-06 12:00 UTC)).await;
    let team = factories::team(&pool, TeamOptions::default()).await;
    let contest = factories::contest_with_games(&pool, "Week 1", &["2025_01_BUF_KC"]).await;
    factories::full_entry(&pool, contest, team.team.id, "00-A").await;
    let g = factories::game_at("2025_01_BUF_KC", 1, datetime!(2025-09-07 17:00 UTC));
    app.nfl.seed_for_test(&[], &[g]).await.unwrap();

    app.login_as(&team.owner).await;
    let body = app.get(&format!("/contests/{contest}")).await.text();
    assert!(!body.contains("You're in"), "no entered badge: {body}");
    assert!(!body.contains("Enter lineup"), "no enter cta: {body}");
    assert!(
        !body.contains(&format!("/contests/{contest}/draft-entry")),
        "no cta button link for entered contest: {body}"
    );
}
