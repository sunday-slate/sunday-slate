use std::collections::BTreeMap;
use std::sync::Arc;

use nfl_data::{
    EspnPlayerId, Game, LiveGamePhase, LiveGameSnapshot, LivePlayerStats, LiveTeamStats,
    PlayerWeekStats, ProviderOutcome, ProviderResponse, Season, TeamAbbr, TeamWeekStats,
};
use sqlx::SqlitePool;
use time::OffsetDateTime;
use time::macros::datetime;

use crate::contests::ContestId;
use crate::live::identity::LiveIdentityIndex;
use crate::live::live_game;
use crate::tests::factories::{self, TeamOptions};
use crate::tests::test_app::TestApp;
use tank01_data::{PollEvent, PollRequest};

use super::entries::setup_staggered_entry;

const KICKOFF: OffsetDateTime = datetime!(2025-09-07 17:00 UTC);
const GAME_ID: &str = "2025_01_BUF_KC";

fn game() -> Game {
    Game {
        kickoff: Some(KICKOFF),
        ..factories::game(GAME_ID, 1)
    }
}

fn live_player(espn_id: &str, name: &str, rushing_yards: i32) -> LivePlayerStats {
    LivePlayerStats {
        espn_id: Some(EspnPlayerId(espn_id.into())),
        name: Some(name.into()),
        team: Some(TeamAbbr("KC".into())),
        position: Some("RB".into()),
        opponent: Some(TeamAbbr("BUF".into())),
        completions: 0,
        attempts: 0,
        passing_tds: 0,
        passing_interceptions: 0,
        rushing_attempts: 1,
        rushing_tds: 0,
        targets: 0,
        receptions: 0,
        receiving_tds: 0,
        fumbles_lost: 0,
        two_point_conversions: 0,
        special_teams_tds: 0,
        fumble_recovery_tds: 0,
        passing_yards: 0,
        rushing_yards,
        receiving_yards: 0,
    }
}
fn live_filler_players() -> Vec<LivePlayerStats> {
    (1..=7)
        .map(|i| live_player(&format!("live-filler-{i}"), &format!("Filler Player{i}"), 0))
        .collect()
}

fn live_defense(team: &str, opponent: &str) -> LiveTeamStats {
    LiveTeamStats {
        team: TeamAbbr(team.into()),
        opponent: TeamAbbr(opponent.into()),
        sacks: 0,
        interceptions: 0,
        fumble_recoveries: 0,
        safeties: 0,
        touchdowns: 0,
        blocked_kicks: 0,
        conversion_returns: 0,
        points_allowed: 24,
        dst_present: true,
        team_stats_present: true,
    }
}

#[allow(clippy::too_many_arguments)]
fn box_poll_event(
    game: &Game,
    sequence: u64,
    observed_at: OffsetDateTime,
    phase: LiveGamePhase,
    period: &str,
    clock: &str,
    home_score: i32,
    away_score: i32,
    players: Vec<LivePlayerStats>,
) -> PollEvent {
    let live_game = live_game(game).expect("fixture kickoff");
    PollEvent::BoxScore {
        sequence,
        game: live_game.clone(),
        request: PollRequest {
            endpoint: "box".into(),
            query: BTreeMap::new(),
        },
        play_by_play: false,
        changed: true,
        response: ProviderResponse {
            requested_at: observed_at,
            received_at: observed_at,
            elapsed_ms: 1,
            http_status: Some(200),
            headers: BTreeMap::new(),
            raw_body: None,
            outcome: ProviderOutcome::Value(LiveGameSnapshot {
                game: live_game,
                observed_at,
                phase,
                period: Some(period.into()),
                clock: Some(clock.into()),
                home_score: Some(home_score),
                away_score: Some(away_score),
                players: Some(players),
                defenses: Some(vec![live_defense("KC", "BUF"), live_defense("BUF", "KC")]),
            }),
        },
    }
}

fn box_score_event(
    game: &Game,
    alpha_yards: i32,
    beta_yards: i32,
    observed_at: OffsetDateTime,
) -> PollEvent {
    box_poll_event(
        game,
        u64::try_from(alpha_yards).unwrap_or_default(),
        observed_at,
        LiveGamePhase::InProgress,
        "2",
        "08:00",
        14,
        7,
        [
            live_player("live-alpha", "Alpha Runner", alpha_yards),
            live_player("live-beta", "Beta Runner", beta_yards),
        ]
        .into_iter()
        .chain(live_filler_players())
        .collect(),
    )
}

fn final_box_score_event(game: &Game, sequence: u64, observed_at: OffsetDateTime) -> PollEvent {
    box_poll_event(
        game,
        sequence,
        observed_at,
        LiveGamePhase::Final,
        "4",
        "00:00",
        28,
        21,
        [
            live_player("live-alpha", "Alpha Runner", 100),
            live_player("live-beta", "Beta Runner", 200),
        ]
        .into_iter()
        .chain(live_filler_players())
        .collect(),
    )
}

fn main_region(html: &str) -> &str {
    html.split_once("<main")
        .and_then(|(_, rest)| rest.split_once("</main>"))
        .map(|(main, _)| main)
        .expect("main renders")
}

#[sqlx::test]
async fn live_contest_ranks_and_drills_into_provisional_scores(pool: SqlitePool) {
    let app = TestApp::from_pool_at(pool.clone(), KICKOFF).await;
    let alpha_team = factories::team(
        &pool,
        TeamOptions {
            name: Some("Gridiron Giants".into()),
            owner_name: Some("Mike".into()),
            ..Default::default()
        },
    )
    .await;
    let beta_team = factories::team(
        &pool,
        TeamOptions {
            league: Some(alpha_team.league.clone()),
            name: Some("Turf Titans".into()),
            owner_name: Some("Sara".into()),
            ..Default::default()
        },
    )
    .await;
    let contest = factories::contest_with_games(&pool, "Week 1", &[GAME_ID]).await;
    let _alpha_entry = factories::full_entry(&pool, contest, alpha_team.team.id, "00-A").await;
    let beta_entry = factories::full_entry(&pool, contest, beta_team.team.id, "00-B").await;

    let mut alpha = factories::player("00-A", "Alpha Runner");
    alpha.espn_id = Some("live-alpha".into());
    let mut beta = factories::player("00-B", "Beta Runner");
    beta.espn_id = Some("live-beta".into());
    let fillers = (1..=7)
        .map(|i| {
            let mut player = factories::player(&format!("00-F{i}"), &format!("Filler Player{i}"));
            player.espn_id = Some(format!("live-filler-{i}"));
            player
        })
        .collect::<Vec<_>>();
    let players = std::iter::once(alpha)
        .chain(std::iter::once(beta))
        .chain(fillers)
        .collect::<Vec<_>>();
    let game = game();
    app.nfl
        .seed_for_test(&players, std::slice::from_ref(&game))
        .await
        .unwrap();

    let identity = LiveIdentityIndex::from_sources(&app.nfl.players().await.unwrap(), &[]);
    let contest_id = ContestId(contest);
    let feed_game = live_game(&game).expect("fixture kickoff");
    app.state
        .live
        .register_contest(contest_id, vec![game.clone()], Arc::new(identity), KICKOFF);
    app.state.live.publish_event(
        contest_id,
        &box_score_event(&game, 100, 200, KICKOFF + time::Duration::seconds(30)),
        std::slice::from_ref(&feed_game),
    );

    app.login_as(&alpha_team.owner).await;
    let body = app.get(&format!("/contests/{contest}")).await.text();
    assert!(
        body.contains(&format!(r#"id="contest-{contest}-live-indicator""#)),
        "contest navbar indicator renders: {body}"
    );
    assert!(
        body.contains(r#"class="size-2 rounded-full bg-success hidden""#),
        "active complete data starts with a hidden green indicator: {body}"
    );
    assert!(body.contains("hx-on::after:sse:connection"), "{body}");
    assert!(body.contains("hx-on::sse:error"), "{body}");
    assert!(body.contains("hx-on::sse:close"), "{body}");
    assert!(!body.contains("Contest scores"), "{body}");
    assert!(!body.contains("score-summary"), "{body}");
    assert!(!body.contains(">LIVE<"), "{body}");
    let contests = app.get("/contests").await.text();
    assert!(
        contests.contains(&format!(r#"href="/contests/{contest}""#)),
        "live contest remains linked in the list: {contests}"
    );
    assert!(
        !contests.contains(">LIVE<"),
        "list has no live badge: {contests}"
    );

    let main = main_region(&body);
    let beta_pos = main.find("Turf Titans").expect("beta row");
    let alpha_pos = main.find("Gridiron Giants").expect("alpha row");
    assert!(
        beta_pos < alpha_pos,
        "higher provisional score ranks first: {body}"
    );
    assert!(
        main.contains("23.00"),
        "beta provisional score renders: {body}"
    );
    assert!(
        main.contains("13.00"),
        "alpha provisional score renders: {body}"
    );
    assert!(
        !main.contains("ph-fill ph-trophy"),
        "provisional scores never crown a winner"
    );
    assert!(
        main.contains("minutes-meter minutes-meter--ok"),
        "meter renders on entry rows: {main}"
    );
    assert!(
        main.contains(r#"style="width: 63%;""#),
        "meter fill at 63% for 342 of 540 minutes: {main}"
    );
    assert!(
        main.contains("342m"),
        "entry meter shows 9 slots x 38 min: {main}"
    );

    let entry_path = format!("/entries/{beta_entry}");
    assert!(body.contains(&format!(r#"href="{entry_path}""#)));
    let entry_body = app.get(&entry_path).await.text();
    assert!(
        entry_body.contains(
            r#"pick-pts-text"><span class="pick-pts-whole">23</span><span class="pick-pts-dec">.00"#
        ),
        "linked lineup total renders: {entry_body}"
    );
    assert!(
        entry_body.contains("200 YDS"),
        "linked lineup stat line renders: {entry_body}"
    );
    for slot in ["qb", "rb1", "rb2", "wr1", "wr2", "wr3", "te", "flex", "def"] {
        assert!(
            entry_body.contains(&format!("id=\"entry-{beta_entry}-{slot}-points\"")),
            "linked lineup renders {slot}: {entry_body}"
        );
    }

    app.state.live.publish_event(
        contest_id,
        &box_score_event(&game, 100, 200, KICKOFF + time::Duration::seconds(45)),
        std::slice::from_ref(&feed_game),
    );
    let replayed = app.get(&entry_path).await.text();
    assert!(
        replayed.contains(
            r#"pick-pts-text"><span class="pick-pts-whole">23</span><span class="pick-pts-dec">.00"#
        ),
        "replaying a snapshot keeps its score"
    );
    assert!(
        !replayed.contains(">46.00<"),
        "replaying a snapshot does not double points"
    );

    app.state.live.publish_event(
        contest_id,
        &box_score_event(&game, 100, 90, KICKOFF + time::Duration::seconds(60)),
        std::slice::from_ref(&feed_game),
    );
    let updated = app.get(&format!("/contests/{contest}")).await.text();
    let updated_main = main_region(&updated);
    let updated_alpha = updated_main.find("Gridiron Giants").expect("alpha row");
    let updated_beta = updated_main.find("Turf Titans").expect("beta row");
    assert!(
        updated_alpha < updated_beta,
        "changed provisional score re-ranks: {updated}"
    );
    assert!(
        updated_main.contains("9.00"),
        "changed beta score renders: {updated}"
    );
    assert!(!updated_main.contains("ph-fill ph-trophy"));

    let buf_player = PlayerWeekStats {
        gsis_id: "00-BUF".into(),
        team: TeamAbbr("BUF".into()),
        opponent: Some(TeamAbbr("KC".into())),
        ..factories::player_stats("00-BUF", 1)
    };
    let buf_defense = TeamWeekStats {
        gsis_game_id: GAME_ID.into(),
        ..factories::defense_stats("BUF", "KC", 1)
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
            &[factories::defense_stats("KC", "BUF", 1), buf_defense],
        )
        .await
        .unwrap();
    let official = app.get(&format!("/contests/{contest}")).await.text();
    let _official_main = main_region(&official);
    assert!(
        !official.contains("Final"),
        "official page has no removed status copy: {official}"
    );
    assert!(
        !official.contains("Contest scores"),
        "official page has no contest score panel: {official}"
    );
    assert!(
        !official.contains("hx-sse:connect"),
        "official page closes live connection"
    );
}

#[sqlx::test]
async fn contest_minutes_follow_staggered_kickoff_windows(pool: SqlitePool) {
    let now = datetime!(2025 - 09 - 07 18:30 UTC);
    let app = TestApp::from_pool_at(pool.clone(), now).await;
    let (owner, entry_id, games) = setup_staggered_entry(&pool, &app).await;
    let contest_id: i64 = sqlx::query_scalar("SELECT contest_id FROM entries WHERE id = ?1")
        .bind(entry_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    let contest = ContestId(contest_id);
    let identity = LiveIdentityIndex::from_sources(&app.nfl.players().await.unwrap(), &[]);
    app.state
        .live
        .register_contest(contest, games.clone(), Arc::new(identity), now);

    let early = &games[0];
    let finished = &games[1];
    let early_live = live_game(early).expect("fixture kickoff");
    let finished_live = live_game(finished).expect("fixture kickoff");
    app.state.live.publish_event(
        contest,
        &box_score_event(early, 100, 200, datetime!(2025 - 09 - 07 17:00:30 UTC)),
        std::slice::from_ref(&early_live),
    );
    app.state.live.publish_event(
        contest,
        &final_box_score_event(finished, 2, now),
        std::slice::from_ref(&finished_live),
    );

    app.login_as(&owner.owner).await;
    let body = app.get(&format!("/contests/{contest_id}")).await.text();
    let main = main_region(&body);
    assert!(
        main.contains("minutes-meter minutes-meter--ok"),
        "staggered meter renders ok while windows mix: {main}"
    );
    assert!(
        main.contains("326m"),
        "early live game: RB1, 5 fillers, DEF at 38 each = 266; scheduled QB 60; final RB2 0: {main}"
    );
    assert!(
        main.contains(r#"style="width: 60%;""#),
        "326 of 540 minutes fills 60%: {main}"
    );

    app.state.live.publish_event(
        contest,
        &final_box_score_event(early, 3, now + time::Duration::minutes(30)),
        std::slice::from_ref(&early_live),
    );
    let drained = app.get(&format!("/contests/{contest_id}")).await.text();
    let drained_main = main_region(&drained);
    assert!(
        drained_main.contains("minutes-meter minutes-meter--warn"),
        "only the scheduled QB's slot keeps the meter in warn: {drained_main}"
    );
    assert!(
        drained_main.contains("60m"),
        "the early game going final leaves just the scheduled QB's 60: {drained_main}"
    );
    assert!(
        drained_main.contains(r#"style="width: 11%;""#),
        "60 of 540 minutes fills 11%: {drained_main}"
    );
    assert!(!drained_main.contains("326m"), "{drained_main}");
}

#[sqlx::test]
async fn dev_feed_snapshot_outranks_official_stats(pool: SqlitePool) {
    let app = TestApp::from_pool_at_with_live_dev_feed(pool.clone(), KICKOFF, true).await;
    let alpha_team = factories::team(
        &pool,
        TeamOptions {
            name: Some("Gridiron Giants".into()),
            owner_name: Some("Mike".into()),
            ..Default::default()
        },
    )
    .await;
    let beta_team = factories::team(
        &pool,
        TeamOptions {
            league: Some(alpha_team.league.clone()),
            name: Some("Turf Titans".into()),
            owner_name: Some("Sara".into()),
            ..Default::default()
        },
    )
    .await;
    let contest = factories::contest_with_games(&pool, "Week 1", &[GAME_ID]).await;
    let _alpha_entry = factories::full_entry(&pool, contest, alpha_team.team.id, "00-A").await;
    let beta_entry = factories::full_entry(&pool, contest, beta_team.team.id, "00-B").await;

    let mut alpha = factories::player("00-A", "Alpha Runner");
    alpha.espn_id = Some("live-alpha".into());
    let mut beta = factories::player("00-B", "Beta Runner");
    beta.espn_id = Some("live-beta".into());
    let fillers = (1..=7)
        .map(|i| {
            let mut player = factories::player(&format!("00-F{i}"), &format!("Filler Player{i}"));
            player.espn_id = Some(format!("live-filler-{i}"));
            player
        })
        .collect::<Vec<_>>();
    let players = std::iter::once(alpha)
        .chain(std::iter::once(beta))
        .chain(fillers)
        .collect::<Vec<_>>();
    let game = game();
    app.nfl
        .seed_for_test(&players, std::slice::from_ref(&game))
        .await
        .unwrap();

    let identity = LiveIdentityIndex::from_sources(&app.nfl.players().await.unwrap(), &[]);
    let contest_id = ContestId(contest);
    let feed_game = live_game(&game).expect("fixture kickoff");
    app.state
        .live
        .register_contest(contest_id, vec![game.clone()], Arc::new(identity), KICKOFF);
    app.state.live.publish_event(
        contest_id,
        &box_score_event(&game, 100, 200, KICKOFF + time::Duration::seconds(30)),
        std::slice::from_ref(&feed_game),
    );

    let buf_player = PlayerWeekStats {
        gsis_id: "00-BUF".into(),
        team: TeamAbbr("BUF".into()),
        opponent: Some(TeamAbbr("KC".into())),
        ..factories::player_stats("00-BUF", 1)
    };
    let buf_defense = TeamWeekStats {
        gsis_game_id: GAME_ID.into(),
        ..factories::defense_stats("BUF", "KC", 1)
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
            &[factories::defense_stats("KC", "BUF", 1), buf_defense],
        )
        .await
        .unwrap();

    app.login_as(&beta_team.owner).await;
    let entry_path = format!("/entries/{beta_entry}");
    let body = app.get(&entry_path).await.text();
    assert!(
        body.contains(
            r#"pick-pts-text"><span class="pick-pts-whole">23</span><span class="pick-pts-dec">.00"#
        ),
        "dev feed provisional score outranks complete official stats: {body}"
    );
    assert!(
        body.contains(&format!(r#"hx-sse:connect="{entry_path}/events""#)),
        "dev feed keeps the live connection open: {body}"
    );
}
