//! Contest list: the Contests tab and the /contests page.

use nfl_data::{
    PlayerWeekStats as NflPlayerWeekStats, Season, SeasonType, TeamAbbr as NflTeamAbbr,
    TeamWeekStats as NflTeamWeekStats, Week,
};
use sqlx::SqlitePool;
use time::macros::datetime;

use crate::tests::factories::{self, TeamOptions};
use crate::tests::test_app::TestApp;

async fn add_contest(pool: &SqlitePool, name: &str) -> i64 {
    sqlx::query_scalar("INSERT INTO contests (name) VALUES (?1) RETURNING id")
        .bind(name)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn add_contest_game(pool: &SqlitePool, contest_id: i64, gsis_game_id: &str) {
    sqlx::query("INSERT INTO contest_games (contest_id, gsis_game_id) VALUES (?1, ?2)")
        .bind(contest_id)
        .bind(gsis_game_id)
        .execute(pool)
        .await
        .unwrap();
}

/// The Contests tab is one of the four fixed bottom-bar tabs: it shows
/// whether or not a slate has been set, unlike the old drawer's gated entry.
#[sqlx::test]
async fn contests_tab_shows_with_no_slate_set(pool: SqlitePool) {
    let app = TestApp::from_pool(pool.clone()).await;
    let fixture = factories::team(&pool, TeamOptions::default()).await;
    app.login_as(&fixture.owner).await;

    let before = app.get("/standings").await;
    before.assert_status_ok();
    assert!(
        before.text().contains(r#"href="/contests""#),
        "the Contests tab shows even without a slate-set contest"
    );

    let contest = add_contest(&pool, "Week 1").await;
    add_contest_game(&pool, contest, "2025_01_BUF_KC").await;

    let after = app.get("/standings").await;
    after.assert_status_ok();
    assert!(
        after.text().contains(r#"href="/contests""#),
        "the Contests tab still shows once a slate is set"
    );
}

fn rb(gsis: &str, week: u8, rush_yards: i32) -> NflPlayerWeekStats {
    NflPlayerWeekStats {
        season: Season(2025),
        week: Week(week),
        season_type: SeasonType::Reg,
        gsis_id: gsis.into(),
        team: NflTeamAbbr("KC".into()),
        opponent: Some(NflTeamAbbr("BUF".into())),
        completions: 0,
        attempts: 0,
        passing_yards: 0,
        passing_tds: 0,
        passing_interceptions: 0,
        rushing_attempts: 0,
        rushing_yards: rush_yards,
        rushing_tds: 0,
        targets: 0,
        receptions: 0,
        receiving_yards: 0,
        receiving_tds: 0,
        fumbles_lost: 0,
        two_point_conversions: 0,
        special_teams_tds: 0,
        fumble_recovery_tds: 0,
        fantasy_points: 0.0,
        fantasy_points_ppr: 0.0,
    }
}

/// Insert a full nine-slot entry; `rb1` is the scored player, the other seven
/// player slots get filler ids with no stat lines (0 points), DEF is "KC"
/// with no team stats seeded (also 0) — the entry's total is its RB1's points.
async fn add_entry(pool: &SqlitePool, contest_id: i64, team_id: i64, rb1: &str) {
    let entry_id: i64 = sqlx::query_scalar(
        "INSERT INTO entries (contest_id, fantasy_team_id) VALUES (?1, ?2) RETURNING id",
    )
    .bind(contest_id)
    .bind(team_id)
    .fetch_one(pool)
    .await
    .unwrap();
    let players = [
        ("QB", "00-F1"),
        ("RB2", "00-F2"),
        ("WR1", "00-F3"),
        ("WR2", "00-F4"),
        ("WR3", "00-F5"),
        ("TE", "00-F6"),
        ("FLEX", "00-F7"),
    ];
    for (slot, gsis) in players.iter().copied().chain([("RB1", rb1)]) {
        sqlx::query(
            "INSERT INTO entry_slots (entry_id, roster_slot, gsis_player_id) VALUES (?1, ?2, ?3)",
        )
        .bind(entry_id)
        .bind(slot)
        .bind(gsis)
        .execute(pool)
        .await
        .unwrap();
    }
    sqlx::query(
        "INSERT INTO entry_slots (entry_id, roster_slot, team_abbr) VALUES (?1, 'DEF', 'KC')",
    )
    .bind(entry_id)
    .execute(pool)
    .await
    .unwrap();
}

#[sqlx::test]
async fn lists_contests_newest_first_with_winners(pool: SqlitePool) {
    let app = TestApp::from_pool_at(
        pool.clone(),
        time::macros::datetime!(2025 - 09 - 10 12:00 UTC),
    )
    .await;

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

    // Week 1 played (both fantasy teams entered); Week 2 slate-set, no
    // entries; Draft Party has no slate at all (no contest_games row).
    let wk1 = add_contest(&pool, "Week 1").await;
    let wk2 = add_contest(&pool, "Week 2").await;
    let _no_slate = add_contest(&pool, "Draft Party").await;
    add_contest_game(&pool, wk1, "2025_01_BUF_KC").await;
    add_contest_game(&pool, wk2, "2025_02_BUF_KC").await;
    add_contest_game(&pool, wk2, "2025_02_NYJ_MIA").await;
    add_entry(&pool, wk1, a.team.id, "00-A").await;
    add_entry(&pool, wk1, b.team.id, "00-B").await;

    app.nfl
        .seed_for_test(
            &[],
            &[
                // 2025-09-08 01:00 UTC is 2025-09-07 21:00 Eastern. This
                // crosses the date boundary on purpose, so the test fails if
                // the handler ever reads the raw UTC date instead of
                // `kickoff_eastern()`. Do not "tidy" it to a daytime value.
                factories::game_at("2025_01_BUF_KC", 1, datetime!(2025-09-08 01:00 UTC)),
                factories::game_at("2025_02_BUF_KC", 2, datetime!(2025-09-14 17:00 UTC)),
                // Same trap on the LATER game of a slate: 2025-09-16 01:00
                // UTC is 2025-09-15 21:00 Eastern.
                factories::game_at("2025_02_NYJ_MIA", 2, datetime!(2025-09-16 01:00 UTC)),
            ],
        )
        .await
        .unwrap();
    // 100 rush yards = 13.00 FanDuel points; 50 = 5.00.
    let buf_player = NflPlayerWeekStats {
        gsis_id: "00-BUF".into(),
        team: NflTeamAbbr("BUF".into()),
        opponent: Some(NflTeamAbbr("KC".into())),
        ..factories::player_stats("00-BUF", 1)
    };
    let buf_defense = NflTeamWeekStats {
        gsis_game_id: "2025_01_BUF_KC".into(),
        ..factories::scoreless_defense_stats("BUF", "KC", 1)
    };
    app.nfl
        .seed_week_stats_for_test(
            Season(2025),
            &[rb("00-A", 1, 100), rb("00-B", 1, 50), buf_player],
            &[
                factories::scoreless_defense_stats("KC", "BUF", 1),
                buf_defense,
            ],
        )
        .await
        .unwrap();

    app.login_as(&a.owner).await;
    let resp = app.get("/contests").await;
    resp.assert_status_ok();
    let html = resp.text();

    // Week 2 (upcoming, Sep 14) sorts above Week 1 (finished, Sep 7).
    let wk2_pos = html.find("Week 2").expect("week 2 row");
    let wk1_pos = html.find("Week 1").expect("week 1 row");
    assert!(wk2_pos < wk1_pos, "newest first: {html}");

    // Week 1 is finished: winner, score, and entry count; runner-up hidden.
    assert!(html.contains("Gridiron Giants"), "winner named: {html}");
    assert!(html.contains("13.00"), "winning score shown: {html}");
    assert!(html.contains("2 entries"), "entry count shown: {html}");
    assert!(!html.contains("Turf Titans"), "runner-up not shown: {html}");

    // Every listed contest is a link into its details page.
    assert!(
        html.contains(&format!(r#"href="/contests/{wk1}""#)),
        "finished contest links to its details page: {html}"
    );
    assert!(
        html.contains(&format!(r#"href="/contests/{wk2}""#)),
        "upcoming contest links to its details page: {html}"
    );

    // Unslated contests never appear.
    assert!(!html.contains("Draft Party"), "unslated hidden: {html}");

    // Week 2 is upcoming: its kickoff (Sep 14 17:00 UTC = 1:00pm Eastern)
    // renders in the center, and no winner is shown for it.
    assert!(html.contains("Sep 14"), "upcoming kickoff date: {html}");
    assert!(
        html.contains("Kickoff: 1:00pm ET"),
        "upcoming kickoff time: {html}"
    );

    // A finished contest with a winner is not muted.
    assert!(
        !html.contains("text-base-content/50\">Week 1<"),
        "finished contest name not muted: {html}"
    );
}
