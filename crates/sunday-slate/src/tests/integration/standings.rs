//! Standings: fantasy teams ranked by sum-of-top-10, computed from entries
//! and nfl-data week stats.

use nfl_data::Season;
use sqlx::SqlitePool;
use time::macros::datetime;

use crate::standings::service::standings_for_league;
use crate::tests::factories::{self, TeamOptions};
use crate::tests::test_app::TestApp;
use crate::tests::utils::list_item_containing;
use nfl_data::{PlayerWeekStats, TeamAbbr, TeamWeekStats};

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

    let buf_player = PlayerWeekStats {
        gsis_id: "00-BUF".into(),
        team: TeamAbbr::BUF,
        opponent: Some(TeamAbbr::KC),
        ..factories::player_stats("00-BUF", 1)
    };
    let buf_defense = TeamWeekStats {
        gsis_game_id: "2025_01_BUF_KC".into(),
        ..factories::scoreless_defense_stats("BUF", "KC", 1)
    };

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
                PlayerWeekStats {
                    opponent: Some(TeamAbbr::BUF),
                    ..factories::rb_stats("00-A", 1, 100)
                },
                PlayerWeekStats {
                    opponent: Some(TeamAbbr::BUF),
                    ..factories::rb_stats("00-B", 1, 200)
                },
                buf_player.clone(),
                PlayerWeekStats {
                    opponent: Some(TeamAbbr::BUF),
                    ..factories::rb_stats("00-A", 2, 50)
                },
                PlayerWeekStats {
                    gsis_id: "00-BUF".into(),
                    team: TeamAbbr::BUF,
                    opponent: Some(TeamAbbr::KC),
                    ..factories::player_stats("00-BUF", 2)
                },
                // Week 3's rows carry no opponent and no BUF-side rows, so
                // the official gate holds the live contest out of the
                // standings until its slate's stats arrive — matching the
                // "Final · awaiting official stats" state the contest list
                // shows for the same contest.
                factories::rb_stats("00-A", 3, 300),
            ],
            &[
                factories::scoreless_defense_stats("KC", "BUF", 1),
                factories::scoreless_defense_stats("KC", "BUF", 2),
                factories::scoreless_defense_stats("KC", "BUF", 3),
                buf_defense.clone(),
                TeamWeekStats {
                    gsis_game_id: "2025_02_BUF_KC".into(),
                    ..factories::scoreless_defense_stats("BUF", "KC", 2)
                },
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

/// The reported bug: a contest that finished this evening joined the
/// standings only after the next calendar day, because the old day-rollover
/// gate classified the slate from Eastern dates. The standings now use the
/// contest page's official-stats gate, so 6pm ET on game day — kickoff 1pm
/// ET, stats synced — the totals and wins are already there.
#[sqlx::test]
async fn final_contest_joins_the_standings_before_day_rollover(pool: SqlitePool) {
    // 6pm ET Sunday: the 1pm ET game is hours past kickoff, but the old
    // day-rollover gate still saw the slate's kickoff date as "today".
    let app = TestApp::from_pool_at(pool.clone(), datetime!(2025-09-07 22:00 UTC)).await;
    let league = factories::bare_league(&pool, None).await;
    let a = factories::team(
        &pool,
        TeamOptions {
            league: Some(league.clone()),
            name: Some("Gridiron Giants".into()),
            owner_name: Some("Mike".into()),
            ..Default::default()
        },
    )
    .await;
    let b = factories::team(
        &pool,
        TeamOptions {
            league: Some(league),
            name: Some("Turf Titans".into()),
            owner_name: Some("Sara".into()),
            ..Default::default()
        },
    )
    .await;

    let wk1 = factories::contest_with_games(&pool, "Week 1", &["2025_01_BUF_KC"]).await;
    factories::full_entry(&pool, wk1, a.team.id, "00-A").await;
    factories::full_entry(&pool, wk1, b.team.id, "00-B").await;

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
    // Official-complete: 100yd = 13.00 for team A, 200yd = 23.00 for team B
    // (contest_details.rs pattern — every player row tagged with its
    // opponent, BUF defense pinned to the slate game's gsis id).
    let buf_player = PlayerWeekStats {
        gsis_id: "00-BUF".into(),
        team: TeamAbbr::BUF,
        opponent: Some(TeamAbbr::KC),
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
                    opponent: Some(TeamAbbr::BUF),
                    ..factories::rb_stats("00-A", 1, 100)
                },
                PlayerWeekStats {
                    opponent: Some(TeamAbbr::BUF),
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
    let resp = app.get("/standings").await;
    resp.assert_status_ok();
    let html = resp.text();
    let titans = html.find("Turf Titans").expect("titans present");
    let giants = html.find("Gridiron Giants").expect("giants present");
    assert!(
        titans < giants,
        "Turf Titans (23.00) ranks above Gridiron Giants (13.00): {html}"
    );
    assert!(html.contains("23.00"), "titans total shown: {html}");
    assert!(html.contains("13.00"), "giants total shown: {html}");

    let rows = standings_for_league(&app.state(), a.league.id)
        .await
        .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].name, "Turf Titans");
    assert_eq!(rows[0].total, Some(23.0));
    assert_eq!(rows[0].rank, Some(1));
    assert_eq!(rows[0].wins, 1, "top score wins the contest");
    assert_eq!(rows[1].name, "Gridiron Giants");
    assert_eq!(rows[1].total, Some(13.0));
    assert_eq!(rows[1].rank, Some(2));
    assert_eq!(rows[1].wins, 0);

    // Same evening, the team page drops its empty state and shows the score.
    let team_html = app.get(&format!("/teams/{}", a.team.id)).await.text();
    assert!(
        team_html.contains("13.00"),
        "team page shows the evening's score: {team_html}"
    );
    assert!(
        !team_html.contains("No scored contests yet."),
        "empty state gone once official stats land: {team_html}"
    );
    assert_eq!(
        team_html.matches("Week 1").count(),
        1,
        "the officially scored contest must appear exactly once on the owner's page — its season row, not an open row: {team_html}"
    );
}

/// The dev live feed owns a registered snapshot for a locked slate: the
/// standings gate must keep the contest out — the same provisional override
/// `live::load_contest_scores` applies before its official check. The
/// contest page deliberately shows replay scores while the flag is on.
#[sqlx::test]
async fn dev_feed_snapshot_keeps_the_standings_official_gate_closed(pool: SqlitePool) {
    use crate::contests::ContestId;
    use std::sync::Arc;

    let now = datetime!(2025-09-07 22:00 UTC);
    let kickoff = now - time::Duration::hours(5); // 17:00 UTC, same shape as the fixture above
    let app = crate::tests::test_app::TestApp::from_pool_at_with_live_dev_feed(
        pool.clone(),
        now,
        true,
    )
    .await;
    // Two teams, contest "Week 1" with game 2025_01_BUF_KC at kickoff,
    // full_entry 00-A (team A) / 00-B (team B) — identical setup to
    // final_contest_joins_the_standings_before_day_rollover, and the same
    // official-complete stat seeding (100yd=13.00 / 200yd=23.00).
    let league = factories::bare_league(&pool, None).await;
    let a = factories::team(
        &pool,
        TeamOptions {
            league: Some(league.clone()),
            name: Some("Gridiron Giants".into()),
            owner_name: Some("Mike".into()),
            ..Default::default()
        },
    )
    .await;
    let b = factories::team(
        &pool,
        TeamOptions {
            league: Some(league),
            name: Some("Turf Titans".into()),
            owner_name: Some("Sara".into()),
            ..Default::default()
        },
    )
    .await;

    let wk1 = factories::contest_with_games(&pool, "Week 1", &["2025_01_BUF_KC"]).await;
    factories::full_entry(&pool, wk1, a.team.id, "00-A").await;
    factories::full_entry(&pool, wk1, b.team.id, "00-B").await;

    let game = factories::game_at("2025_01_BUF_KC", 1, kickoff);
    app.nfl
        .seed_for_test(&[], std::slice::from_ref(&game))
        .await
        .unwrap();
    // Official-complete: 100yd = 13.00 for team A, 200yd = 23.00 for team B
    // (contest_details.rs pattern — every player row tagged with its
    // opponent, BUF defense pinned to the slate game's gsis id).
    let buf_player = PlayerWeekStats {
        gsis_id: "00-BUF".into(),
        team: TeamAbbr::BUF,
        opponent: Some(TeamAbbr::KC),
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
                    opponent: Some(TeamAbbr::BUF),
                    ..factories::rb_stats("00-A", 1, 100)
                },
                PlayerWeekStats {
                    opponent: Some(TeamAbbr::BUF),
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

    // Run the dev feed once so the snapshot registers for this locked contest
    // (kickoff is 5 wall-clock hours before `now`, inside the feed's 12-hour
    // recovery window). Drain it like dev_feed.rs's own test does: tick 1ms
    // instead of the 3s default, so `run()` settles instead of sleeping
    // DEV_TICKS×3s of wall clock.
    let contest_id = ContestId(wk1);
    let mut config = (*app.state.config).clone();
    config.live_dev_feed_tick_ms = 1;
    let state = crate::AppState {
        config: Arc::new(config),
        ..app.state.clone()
    };
    crate::live::dev_feed::run(state.clone()).await;

    // The dev feed's own test (live/dev_feed.rs:883) proves a finished game
    // publishes phase Final and real finals — with that snapshot registered,
    // the gate must still refuse to admit:
    assert!(
        state.live.snapshot(contest_id).is_some(),
        "the dev feed must have registered the snapshot for this locked contest"
    );
    let rows = crate::standings::service::standings_for_league(&app.state(), a.league.id)
        .await
        .unwrap();
    assert_eq!(rows.len(), 2);
    for row in rows {
        assert!(
            row.total.is_none(),
            "{} must stay unscored while the dev feed owns the snapshot: rank={} total={} wins={}",
            row.name,
            row.rank.map(|r| r.to_string()).unwrap_or_else(|| "-".into()),
            row.total.map(|t| t.to_string()).unwrap_or_else(|| "-".into()),
            row.wins,
        );
    }
    // And the owner page keeps the empty state (the dev feed's own Live row,
    // not a season row).
    app.login_as(&a.owner).await;
    let team_html = app.get(&format!("/teams/{}", a.team.id)).await.text();
    assert!(
        team_html.contains("No scored contests yet."),
        "dev-feed-owned contest stays out of the season list: {team_html}"
    );
}

/// The pending window: the contest is over, but the slate's official stats
/// have not fully arrived (no opponent tags, no BUF rows — what a partial
/// nflverse sync yields). The standings must not score the lineup a false
/// zero; the contest stays out until the gate admits it.
#[sqlx::test]
async fn a_contest_awaiting_official_stats_stays_out_of_the_standings(pool: SqlitePool) {
    // Well past the slate's last kickoff, but the stats are incomplete.
    let app = TestApp::from_pool_at(pool.clone(), datetime!(2025-09-08 20:00 UTC)).await;
    let league = factories::bare_league(&pool, None).await;
    let a = factories::team(
        &pool,
        TeamOptions {
            league: Some(league.clone()),
            name: Some("Gridiron Giants".into()),
            owner_name: Some("Mike".into()),
            ..Default::default()
        },
    )
    .await;
    let b = factories::team(
        &pool,
        TeamOptions {
            league: Some(league),
            name: Some("Turf Titans".into()),
            owner_name: Some("Sara".into()),
            ..Default::default()
        },
    )
    .await;

    let wk1 = factories::contest_with_games(&pool, "Week 1", &["2025_01_BUF_KC"]).await;
    factories::full_entry(&pool, wk1, a.team.id, "00-A").await;
    factories::full_entry(&pool, wk1, b.team.id, "00-B").await;

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
    // Opponent-less player rows and only the KC defense: the official gate
    // rejects the slate.
    app.nfl
        .seed_week_stats_for_test(
            Season(2025),
            &[
                factories::rb_stats("00-A", 1, 100),
                factories::rb_stats("00-B", 1, 200),
            ],
            &[factories::scoreless_defense_stats("KC", "BUF", 1)],
        )
        .await
        .unwrap();

    app.login_as(&a.owner).await;
    let resp = app.get("/standings").await;
    resp.assert_status_ok();
    let html = resp.text();
    assert!(
        html.contains("Gridiron Giants"),
        "both teams listed: {html}"
    );
    assert!(html.contains("Turf Titans"), "both teams listed: {html}");
    assert!(
        !html.contains("23.00") && !html.contains("18.00"),
        "no totals while stats are pending: {html}"
    );

    let rows = standings_for_league(&app.state(), a.league.id)
        .await
        .unwrap();
    assert_eq!(rows.len(), 2);
    for row in rows {
        assert!(
            row.total.is_none(),
            "{} has no total while stats are pending",
            row.name
        );
        assert!(
            row.rank.is_none(),
            "{} has no rank while stats are pending",
            row.name
        );
    }
}
