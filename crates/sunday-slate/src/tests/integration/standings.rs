//! Standings: fantasy teams ranked by sum-of-top-10, computed from entries
//! and nfl-data week stats.

use nfl_data::Season;
use sqlx::SqlitePool;
use time::macros::datetime;

use crate::standings::service::standings_for_league;
use crate::tests::factories::{self, TeamOptions};
use crate::tests::test_app::TestApp;
use crate::tests::utils::list_item_containing;

#[sqlx::test]
async fn ranks_teams_by_top_scores(pool: SqlitePool) {
    let app = TestApp::from_pool_at(pool.clone(), datetime!(2025-09-15 18:00 UTC)).await;

    // One league, three fantasy teams.
    let a = factories::team(
        &pool,
        TeamOptions {
            name: Some("Gridiron Giants".into()),
            owner_name: Some("Mike".into()),
            ..Default::default()
        },
    )
    .await;
    let league = a.league.clone();
    let b = factories::team(
        &pool,
        TeamOptions {
            league: Some(league.clone()),
            name: Some("Turf Titans".into()),
            owner_name: Some("Sara".into()),
            ..Default::default()
        },
    )
    .await;
    let c = factories::team(
        &pool,
        TeamOptions {
            league: Some(league.clone()),
            name: Some("Bench Warmers".into()),
            owner_name: Some("Lee".into()),
            ..Default::default()
        },
    )
    .await;

    // Two finished contests and one same-day live contest, each with one
    // materialized game fixing its NFL week.
    let wk1 = factories::contest_with_games(&pool, "Week 1", &["2025_01_BUF_KC"]).await;
    let wk2 = factories::contest_with_games(&pool, "Week 2", &["2025_02_BUF_KC"]).await;
    let wk_live = factories::contest_with_games(&pool, "Live Week", &["2025_03_BUF_KC"]).await;

    // A fourth contest whose materialized game is NOT among the games seeded
    // via `nfl.seed_for_test` below, so `nfl.games(season)` can never map it
    // to an NFL week. The handler must skip entries in unresolvable contests
    // rather than default them to some week and score them.
    let wk_unresolved = factories::contest(&pool, "Unscheduled Week").await;
    sqlx::query(
        "INSERT INTO contest_games (contest_id, gsis_game_id) VALUES (?1, '2025_99_BUF_KC')",
    )
    .bind(wk_unresolved)
    .execute(&pool)
    .await
    .unwrap();

    // Team A picks RB "A" (wk1: 100yd=13.0, wk2: 50yd=5.0 -> total 18.00).
    // Team B picks RB "B" (wk1: 200yd=23.0 -> total 23.00).
    // Team C only enters the unresolvable contest -> must stay unscored.
    // Team A's live entry would reverse the order if live points leaked in.
    factories::full_entry(&pool, wk1, a.team.id, "00-A").await;
    factories::full_entry(&pool, wk2, a.team.id, "00-A").await;
    factories::full_entry(&pool, wk_live, a.team.id, "00-A").await;
    factories::full_entry(&pool, wk1, b.team.id, "00-B").await;
    factories::full_entry(&pool, wk_unresolved, c.team.id, "00-C").await;

    app.nfl
        .seed_for_test(
            &[],
            &[
                factories::game_at("2025_01_BUF_KC", 1, datetime!(2025-09-07 17:00 UTC)),
                factories::game_at("2025_02_BUF_KC", 2, datetime!(2025-09-14 17:00 UTC)),
                factories::game_at("2025_03_BUF_KC", 3, datetime!(2025-09-15 17:00 UTC)),
            ],
        )
        .await
        .unwrap();
    app.nfl
        .seed_week_stats_for_test(
            Season(2025),
            &[
                factories::rb_stats("00-A", 1, 100),
                factories::rb_stats("00-B", 1, 200),
                factories::rb_stats("00-A", 2, 50),
                factories::rb_stats("00-A", 3, 300),
            ],
            &[
                factories::scoreless_defense_stats("KC", "BUF", 1),
                factories::scoreless_defense_stats("KC", "BUF", 2),
                factories::scoreless_defense_stats("KC", "BUF", 3),
            ],
        )
        .await
        .unwrap();

    app.login_as(&a.owner).await;
    let resp = app.get("/standings").await;
    resp.assert_status_ok();
    let html = resp.text();

    assert!(html.contains(">Standings<"), "page title: {html}");

    // Turf Titans (23.00) ranked above Gridiron Giants (18.00); Bench Warmers
    // last, blank — even though it has an entry, that entry lives in a
    // contest whose week can't be resolved, so it must not be scored.
    let giants = html.find("Gridiron Giants").expect("giants present");
    let titans = html.find("Turf Titans").expect("titans present");
    let bench = html.find("Bench Warmers").expect("bench present");
    let giants_item = list_item_containing(&html, &format!(r#"href="/teams/{}""#, a.team.id));
    let titans_item = list_item_containing(&html, &format!(r#"href="/teams/{}""#, b.team.id));
    let bench_item = list_item_containing(&html, &format!(r#"href="/teams/{}""#, c.team.id));
    assert!(
        titans < giants,
        "Turf Titans (23) above Gridiron Giants (18)"
    );
    assert!(giants < bench, "unscored Bench Warmers sorts last");
    assert!(
        giants_item.contains("bg-primary/10"),
        "owned row rests green"
    );
    assert!(
        giants_item.contains("hover:bg-primary/20"),
        "owned row strengthens on hover"
    );
    for (name, item) in [("Turf Titans", titans_item), ("Bench Warmers", bench_item)] {
        assert!(!item.contains("bg-primary/10"), "{name} is not highlighted");
        assert!(
            !item.contains("hover:bg-primary/20"),
            "{name} has no owned hover"
        );
        assert!(
            item.contains("hover:bg-base-200"),
            "{name} keeps ordinary hover"
        );
    }
    assert!(html.contains("23.00"), "titans total shown: {html}");
    assert!(html.contains("18.00"), "giants total shown");
    // If contest -> week resolution regressed (e.g. defaulting to a week
    // instead of skipping), Bench Warmers' entry would score 0.0 and the
    // team would become "scored" with a 0.00 total, ranked above the
    // still-unscored teams instead of sorting last by name.
    assert!(
        !html.contains("0.00"),
        "skipped entry must not score its team a zero: {html}"
    );
}

#[sqlx::test]
async fn pre_week_one_lists_every_team_without_scores_or_ranks(pool: SqlitePool) {
    let app = TestApp::from_pool_at(pool.clone(), datetime!(2025-09-06 12:00 UTC)).await;
    let league = factories::bare_league(&pool, None).await;
    let entered = factories::team(
        &pool,
        TeamOptions {
            league: Some(league.clone()),
            name: Some("Zulu United".into()),
            owner_name: Some("Zoe".into()),
            ..Default::default()
        },
    )
    .await;
    let _unentered = factories::team(
        &pool,
        TeamOptions {
            league: Some(league.clone()),
            name: Some("Alpha Aces".into()),
            owner_name: Some("Amy".into()),
            ..Default::default()
        },
    )
    .await;

    let wk1 = factories::contest_with_games(&pool, "Week 1", &["2025_01_BUF_KC"]).await;
    factories::full_entry(&pool, wk1, entered.team.id, "00-A").await;
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

    app.login_as(&entered.owner).await;
    let resp = app.get("/standings").await;
    resp.assert_status_ok();
    let html = resp.text();

    let alpha = html.find("Alpha Aces").expect("unentered team present");
    let zulu = html.find("Zulu United").expect("entered team present");
    assert!(alpha < zulu, "unscored teams sort by name: {html}");
    assert!(html.contains("Amy"), "unentered owner present: {html}");
    assert!(html.contains("Zoe"), "entered owner present: {html}");
    assert!(
        !html.contains("0.00"),
        "preseason total stays blank: {html}"
    );
    assert!(!html.contains(" win"), "preseason wins stay blank: {html}");

    let rows = standings_for_league(&app.state(), entered.league.id)
        .await
        .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].name, "Alpha Aces");
    assert_eq!(rows[1].name, "Zulu United");
    for row in rows {
        assert!(row.rank.is_none(), "{} has no rank", row.name);
        assert!(row.total.is_none(), "{} has no total", row.name);
        assert_eq!(row.wins, 0, "{} has no wins", row.name);
    }
}
