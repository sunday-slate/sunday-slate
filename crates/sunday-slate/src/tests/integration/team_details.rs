//! Team detail page: one fantasy team's season scores by contest.

use axum::http::StatusCode;
use nfl_data::Season;
use sqlx::SqlitePool;
use time::macros::datetime;

use crate::tests::factories::{self, TeamOptions};
use crate::tests::test_app::TestApp;
use crate::tests::utils::list_item_containing;

#[sqlx::test]
async fn member_views_another_teams_season(pool: SqlitePool) {
    // Every kickoff below lands in September 2025; "now" is well after the
    // last of them, so all three contests read FINISHED.
    let app = TestApp::from_pool_at(pool.clone(), datetime!(2025-10-01 12:00 UTC)).await;
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
            ..Default::default()
        },
    )
    .await;

    // Three contests, each pinned to an NFL week by one materialized game.
    let wk1 = factories::contest(&pool, "Week 1").await;
    let wk2 = factories::contest(&pool, "Week 2").await;
    let wk3 = factories::contest(&pool, "Week 3").await;
    for (c, gid) in [
        (wk1, "2025_01_BUF_KC"),
        (wk2, "2025_02_BUF_KC"),
        (wk3, "2025_03_BUF_KC"),
    ] {
        sqlx::query("INSERT INTO contest_games (contest_id, gsis_game_id) VALUES (?1, ?2)")
            .bind(c)
            .bind(gid)
            .execute(&pool)
            .await
            .unwrap();
    }
    factories::full_entry(&pool, wk1, a.team.id, "00-A").await;
    factories::full_entry(&pool, wk2, a.team.id, "00-A").await;
    factories::full_entry(&pool, wk3, a.team.id, "00-A").await;

    app.nfl
        .seed_for_test(
            &[],
            &[
                factories::game_at("2025_01_BUF_KC", 1, datetime!(2025-09-07 17:00 UTC)),
                factories::game_at("2025_02_BUF_KC", 2, datetime!(2025-09-14 17:00 UTC)),
                factories::game_at("2025_03_BUF_KC", 3, datetime!(2025-09-21 17:00 UTC)),
            ],
        )
        .await
        .unwrap();
    // Week 1: 100yd = 13.0. Week 2: 50yd = 5.0. Week 3: 200yd = 23.0.
    app.nfl
        .seed_week_stats_for_test(
            Season(2025),
            &[
                factories::rb_stats("00-A", 1, 100),
                factories::rb_stats("00-A", 2, 50),
                factories::rb_stats("00-A", 3, 200),
            ],
            &[
                factories::scoreless_defense_stats("KC", "BUF", 1),
                factories::scoreless_defense_stats("KC", "BUF", 2),
                factories::scoreless_defense_stats("KC", "BUF", 3),
            ],
        )
        .await
        .unwrap();

    // Another member (not the owner) can view the page.
    app.login_as(&b.owner).await;
    let resp = app.get(&format!("/teams/{}", a.team.id)).await;
    resp.assert_status_ok();
    let html = resp.text();
    assert!(html.contains("Gridiron Giants"));
    assert!(html.contains("Mike"));
    assert!(html.contains("41.00"), "season total 23+13+5: {html}");
    assert!(
        html.contains("season total") && !html.contains("best-10 total"),
        "label stays season total under 11 scored contests: {html}"
    );
    let w3 = html.find("Week 3").expect("week 3 row");
    let w1 = html.find("Week 1").expect("week 1 row");
    let w2 = html.find("Week 2").expect("week 2 row");
    assert!(w3 < w1 && w1 < w2, "rows ordered by score descending");
    assert!(
        !html.contains("Score to beat"),
        "no mark under ten scores: {html}"
    );
    let edit_links = [
        format!(r#"href="/teams/{}/edit""#, a.team.id),
        format!(r#"href="/teams/{}/logo""#, a.team.id),
    ];
    for link in &edit_links {
        assert!(!html.contains(link), "another member sees {link}: {html}");
    }

    app.login_as(&a.owner).await;
    let owner_page = app.get(&format!("/teams/{}", a.team.id)).await;
    owner_page.assert_status_ok();
    let owner_html = owner_page.text();
    for link in &edit_links {
        assert!(
            owner_html.contains(link),
            "owner lacks {link}: {owner_html}"
        );
    }
}

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

#[sqlx::test]
async fn owner_sees_entry_link_for_an_upcoming_contest(pool: SqlitePool) {
    // Slate kicks off 2025-09-07 17:00 UTC; "now" is three days before.
    let app = TestApp::from_pool_at(pool.clone(), datetime!(2025-09-04 17:00 UTC)).await;
    let a = factories::team(&pool, TeamOptions::default()).await;
    let wk1 = factories::contest_with_games(&pool, "Week 1", &["2025_01_BUF_KC"]).await;
    app.nfl
        .seed_for_test(
            &[],
            &[factories::game_at(
                "2025_01_BUF_KC",
                1,
                datetime!(2025-09-07 17:00 UTC),
            )],
        )
        .await
        .unwrap();
    let entry = factories::full_entry(&pool, wk1, a.team.id, "00-A").await;

    app.login_as(&a.owner).await;
    let resp = app.get(&format!("/teams/{}", a.team.id)).await;
    resp.assert_status_ok();
    let html = resp.text();
    let row = list_item_containing(&html, &format!(r#"href="/entries/{entry}""#));
    assert!(row.contains("Week 1"), "{html}");
    assert!(row.contains("Kicks off"), "{html}");
    assert!(row.contains("locks in"), "{html}");
    assert!(
        html.contains("No scored contests yet."),
        "season list below stays empty pre-kickoff: {html}"
    );
}

#[sqlx::test]
async fn owner_sees_enter_button_when_not_entered(pool: SqlitePool) {
    let app = TestApp::from_pool_at(pool.clone(), datetime!(2025-09-04 17:00 UTC)).await;
    let a = factories::team(&pool, TeamOptions::default()).await;
    let wk1 = factories::contest_with_games(&pool, "Week 1", &["2025_01_BUF_KC"]).await;
    app.nfl
        .seed_for_test(
            &[],
            &[factories::game_at(
                "2025_01_BUF_KC",
                1,
                datetime!(2025-09-07 17:00 UTC),
            )],
        )
        .await
        .unwrap();

    app.login_as(&a.owner).await;
    let html = app.get(&format!("/teams/{}", a.team.id)).await.text();
    assert!(
        html.contains(&format!(r#"href="/contests/{wk1}/draft-entry""#)),
        "{html}"
    );
    assert!(html.contains("Enter lineup"), "{html}");
}

#[sqlx::test]
async fn owner_resumes_a_started_draft_from_the_team_page(pool: SqlitePool) {
    let app = TestApp::from_pool_at(pool.clone(), datetime!(2025-09-04 17:00 UTC)).await;
    let a = factories::team(&pool, TeamOptions::default()).await;
    let wk1 = factories::contest_with_games(&pool, "Week 1", &["2025_01_BUF_KC"]).await;
    app.nfl
        .seed_for_test(
            &[],
            &[factories::game_at(
                "2025_01_BUF_KC",
                1,
                datetime!(2025-09-07 17:00 UTC),
            )],
        )
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO draft_entries (contest_id, fantasy_team_id, qb_gsis_id) VALUES (?1, ?2, '00-A')",
    )
    .bind(wk1)
    .bind(a.team.id)
    .execute(&pool)
    .await
    .unwrap();

    app.login_as(&a.owner).await;
    let html = app.get(&format!("/teams/{}", a.team.id)).await.text();
    assert!(html.contains("Resume lineup"), "{html}");
    assert!(
        !html.contains("Enter lineup"),
        "a partial draft should not also offer Enter: {html}"
    );
}

#[sqlx::test]
async fn live_entered_contest_links_to_the_contest(pool: SqlitePool) {
    // Kickoff was 2025-09-07 17:00 UTC; "now" is later that evening: live.
    let app = TestApp::from_pool_at(pool.clone(), datetime!(2025-09-07 20:00 UTC)).await;
    let a = factories::team(&pool, TeamOptions::default()).await;
    let wk1 = factories::contest_with_games(&pool, "Week 1", &["2025_01_BUF_KC"]).await;
    app.nfl
        .seed_for_test(
            &[],
            &[factories::game_at(
                "2025_01_BUF_KC",
                1,
                datetime!(2025-09-07 17:00 UTC),
            )],
        )
        .await
        .unwrap();
    factories::full_entry(&pool, wk1, a.team.id, "00-A").await;

    app.login_as(&a.owner).await;
    let html = app.get(&format!("/teams/{}", a.team.id)).await.text();
    let row = list_item_containing(&html, &format!(r#"href="/contests/{wk1}""#));
    assert!(row.contains("Week 1"), "{html}");
    assert!(!row.contains("LIVE"), "{html}");
    assert!(!html.contains("/draft-entry"), "entries are locked: {html}");
}

/// The `<main>...</main>` region of a rendered page, so a contest name can be
/// counted without also matching the page chrome (nav labels, titles, etc.).
fn main_region(html: &str) -> &str {
    let start = html.find("<main").expect("main region");
    let end = html[start..]
        .find("</main>")
        .map(|offset| start + offset)
        .expect("main region closes");
    &html[start..end]
}

#[sqlx::test]
async fn live_contest_with_loaded_stats_still_appears_once(pool: SqlitePool) {
    // Kickoff was 2025-09-07 17:00 UTC; "now" is later that evening: live,
    // and the week's stats have already started loading mid-slate — the
    // scenario that used to double-render the contest (open section + season
    // list) before the season list gained its FINISHED gate.
    let app = TestApp::from_pool_at(pool.clone(), datetime!(2025-09-07 20:00 UTC)).await;
    let a = factories::team(&pool, TeamOptions::default()).await;
    let wk1 = factories::contest_with_games(&pool, "Week 1", &["2025_01_BUF_KC"]).await;
    app.nfl
        .seed_for_test(
            &[],
            &[factories::game_at(
                "2025_01_BUF_KC",
                1,
                datetime!(2025-09-07 17:00 UTC),
            )],
        )
        .await
        .unwrap();
    factories::full_entry(&pool, wk1, a.team.id, "00-A").await;
    app.nfl
        .seed_week_stats_for_test(
            Season(2025),
            &[factories::rb_stats("00-A", 1, 100)],
            &[factories::scoreless_defense_stats("KC", "BUF", 1)],
        )
        .await
        .unwrap();

    app.login_as(&a.owner).await;
    let html = app.get(&format!("/teams/{}", a.team.id)).await.text();
    let main = main_region(&html);
    assert_eq!(
        main.matches("Week 1").count(),
        1,
        "the live contest must appear exactly once, not also in the season list: {html}"
    );
    let row = list_item_containing(&html, &format!(r#"href="/contests/{wk1}""#));
    assert!(row.contains("Week 1"), "the one row remains linked: {html}");
    assert!(!row.contains("LIVE"), "the live badge is removed: {html}");
}

#[sqlx::test]
async fn other_members_see_no_open_contests_section(pool: SqlitePool) {
    let app = TestApp::from_pool_at(pool.clone(), datetime!(2025-09-04 17:00 UTC)).await;
    let a = factories::team(&pool, TeamOptions::default()).await;
    let b = factories::team(
        &pool,
        TeamOptions {
            league: Some(a.league.clone()),
            ..Default::default()
        },
    )
    .await;
    let wk1 = factories::contest_with_games(&pool, "Week 1", &["2025_01_BUF_KC"]).await;
    app.nfl
        .seed_for_test(
            &[],
            &[factories::game_at(
                "2025_01_BUF_KC",
                1,
                datetime!(2025-09-07 17:00 UTC),
            )],
        )
        .await
        .unwrap();
    let entry = factories::full_entry(&pool, wk1, a.team.id, "00-A").await;

    app.login_as(&b.owner).await;
    let resp = app.get(&format!("/teams/{}", a.team.id)).await;
    resp.assert_status_ok();
    let html = resp.text();
    assert!(!html.contains("Open Contests"), "{html}");
    assert!(!html.contains("/draft-entry"), "{html}");
    assert!(
        !html.contains(&format!(r#"href="/entries/{entry}""#)),
        "another member must not reach the pre-lock lineup from here: {html}"
    );
}

/// A contest that has finished but whose week nflverse has not synced yet
/// scores nothing, rather than the false 0.00 an empty stat set used to
/// produce. The Monday-morning window between kickoff and the sync.
#[sqlx::test]
async fn finished_contest_with_unsynced_stats_scores_nothing(pool: SqlitePool) {
    // Two days past the slate's only kickoff: finished, and no week stats
    // are seeded, so nflverse has nothing to report for week 1.
    let app = TestApp::from_pool_at(pool.clone(), datetime!(2025-09-09 17:00 UTC)).await;
    let a = factories::team(&pool, TeamOptions::default()).await;
    let wk1 = factories::contest_with_games(&pool, "Week 1", &["2025_01_BUF_KC"]).await;
    app.nfl
        .seed_for_test(
            &[],
            &[factories::game_at(
                "2025_01_BUF_KC",
                1,
                datetime!(2025-09-07 17:00 UTC),
            )],
        )
        .await
        .unwrap();
    factories::full_entry(&pool, wk1, a.team.id, "00-A").await;

    app.login_as(&a.owner).await;
    let html = app.get(&format!("/teams/{}", a.team.id)).await.text();
    assert!(
        html.contains("No scored contests yet."),
        "unsynced week must not score: {html}"
    );
    assert!(
        !html.contains("0.00"),
        "no false zero for the unsynced week: {html}"
    );
}

#[sqlx::test]
async fn finished_contests_with_partial_stat_feeds_remain_unscored(pool: SqlitePool) {
    let app = TestApp::from_pool_at(pool.clone(), datetime!(2025-09-16 17:00 UTC)).await;
    let owner = factories::team(
        &pool,
        TeamOptions {
            name: Some("Gridiron Giants".into()),
            ..Default::default()
        },
    )
    .await;
    let player_only =
        factories::contest_with_games(&pool, "Player Feed Only", &["2025_01_BUF_KC"]).await;
    let defense_only =
        factories::contest_with_games(&pool, "Defense Feed Only", &["2025_02_BUF_KC"]).await;
    factories::full_entry(&pool, player_only, owner.team.id, "00-A").await;
    factories::full_entry(&pool, defense_only, owner.team.id, "00-A").await;

    app.nfl
        .seed_for_test(
            &[],
            &[
                factories::game_at("2025_01_BUF_KC", 1, datetime!(2025-09-07 17:00 UTC)),
                factories::game_at("2025_02_BUF_KC", 2, datetime!(2025-09-14 17:00 UTC)),
            ],
        )
        .await
        .unwrap();
    app.nfl
        .seed_week_stats_for_test(
            Season(2025),
            &[factories::rb_stats("00-A", 1, 100)],
            &[factories::defense_stats("KC", "BUF", 2)],
        )
        .await
        .unwrap();

    app.login_as(&owner.owner).await;
    let resp = app.get(&format!("/teams/{}", owner.team.id)).await;
    resp.assert_status_ok();
    let html = resp.text();
    let main = main_region(&html);
    assert!(main.contains("No scored contests yet."), "{html}");
    assert!(
        !main.contains("Player Feed Only"),
        "player-only contest row: {html}"
    );
    assert!(
        !main.contains("Defense Feed Only"),
        "defense-only contest row: {html}"
    );
    assert!(!main.contains("13.00"), "player-only partial score: {html}");
    assert!(
        !main.contains("10.00"),
        "defense-only partial score: {html}"
    );
}
