//! Entry details page: one entry's scored lineup, with access rules.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::Body;
use http::{Request, StatusCode};
use http_body_util::BodyExt;
use nfl_data::{
    EspnPlayerId, Game, LiveGamePhase, LiveGameSnapshot, LivePlayerStats, LiveScoreboard,
    LiveScoreboardGame, LiveTeamStats, PlayerWeekStats as NflPlayerWeekStats, ProviderOutcome,
    ProviderResponse, Season, TeamAbbr as NflTeamAbbr, TeamWeekStats as NflTeamWeekStats,
};
use sqlx::SqlitePool;
use time::OffsetDateTime;
use time::macros::datetime;
use tower::ServiceExt;

use crate::live::live_game;
use crate::media::{self, store as media_store};
use crate::nfl_players::store as player_store;
use crate::nfl_teams::store as team_store;
use crate::tests::factories::{self, GeneratedTeam, TeamOptions};
use crate::tests::test_app::TestApp;
use crate::tests::utils::{as_rendered, href_before, png_bytes};

/// The tab bar's active-tab marker (mirrors `tests::integration::chrome`).
const ACTIVE: &str = r#"aria-current="page""#;

/// The one game on the contest's slate.
const SLATE_GAME: &str = "2025_01_BUF_KC";

/// Kickoff: Sunday 2025-09-07 13:00 ET.
const KICKOFF: OffsetDateTime = datetime!(2025-09-07 17:00 UTC);

/// One hour before kickoff.
const BEFORE_KICKOFF: OffsetDateTime = datetime!(2025-09-07 16:00 UTC);

fn scheduled_game() -> Game {
    Game {
        kickoff: Some(KICKOFF),
        ..factories::game(SLATE_GAME, 1)
    }
}

fn live_scoreboard_event(game: &Game) -> tank01_data::PollEvent {
    let live_game = live_game(game).expect("fixture kickoff");
    tank01_data::PollEvent::Scoreboard {
        sequence: 1,
        request: tank01_data::PollRequest {
            endpoint: "scoreboard".into(),
            query: BTreeMap::new(),
        },
        response: ProviderResponse {
            requested_at: KICKOFF,
            received_at: KICKOFF,
            elapsed_ms: 1,
            http_status: Some(200),
            headers: BTreeMap::new(),
            raw_body: None,
            outcome: ProviderOutcome::Value(LiveScoreboard {
                games: vec![LiveScoreboardGame {
                    provider_game_id: live_game.provider_game_id,
                    phase: LiveGamePhase::InProgress,
                    period: Some("2".into()),
                    clock: Some("08:00".into()),
                    home_team: live_game.home_team,
                    away_team: live_game.away_team,
                    home_score: Some(14),
                    away_score: Some(7),
                }],
            }),
        },
    }
}

fn live_player_stats(
    espn_id: &str,
    name: &str,
    team: &str,
    opponent: &str,
    position: &str,
    rushing_yards: i32,
) -> LivePlayerStats {
    LivePlayerStats {
        espn_id: Some(EspnPlayerId(espn_id.into())),
        name: Some(name.into()),
        team: Some(NflTeamAbbr(team.into())),
        position: Some(position.into()),
        opponent: Some(NflTeamAbbr(opponent.into())),
        completions: 0,
        attempts: 0,
        passing_tds: 0,
        passing_interceptions: 0,
        rushing_attempts: u32::from(rushing_yards > 0),
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

fn box_score_event(
    game: &Game,
    sequence: u64,
    phase: LiveGamePhase,
    observed_at: OffsetDateTime,
    period: &str,
    clock: &str,
    players: Vec<LivePlayerStats>,
) -> tank01_data::PollEvent {
    let live_game = live_game(game).expect("fixture kickoff");
    tank01_data::PollEvent::BoxScore {
        sequence,
        game: live_game.clone(),
        request: tank01_data::PollRequest {
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
                home_score: Some(14),
                away_score: Some(7),
                players: Some(players),
                defenses: Some(vec![]),
            }),
        },
    }
}

fn unknown_live_player_stats() -> LivePlayerStats {
    live_player_stats("unknown-player", "Unknown Player", "KC", "BUF", "RB", 0)
}

struct SseReader {
    body: Body,
    pending: Vec<u8>,
}

impl SseReader {
    fn new(body: Body) -> Self {
        Self {
            body,
            pending: Vec::new(),
        }
    }

    async fn next_event(&mut self) -> Option<String> {
        loop {
            if let Some(end) = self.pending.windows(2).position(|window| window == b"\n\n") {
                let event = self.pending.drain(..end + 2).collect::<Vec<_>>();
                return Some(String::from_utf8(event).expect("SSE event is UTF-8"));
            }

            let frame = self.body.frame().await;
            let frame = frame?;
            let frame = frame.expect("SSE body frame");
            if let Ok(data) = frame.into_data() {
                self.pending.extend_from_slice(&data);
            }
        }
    }
}

/// Two fantasy teams in one league, a "Week 1" contest whose slate is
/// `SLATE_GAME`, and an entry for team `a` with `rb1` at RB1. The seven
/// filler picks are named in nfl-data; `scheduled` is the schedule nfl-data
/// holds. Returns (owner team, other-member team, entry id).
async fn setup_entry(
    pool: &SqlitePool,
    app: &TestApp,
    rb1: &str,
    scheduled: &[Game],
) -> (GeneratedTeam, GeneratedTeam, i64) {
    let a = factories::team(
        pool,
        TeamOptions {
            name: Some("Gridiron Giants".into()),
            ..Default::default()
        },
    )
    .await;
    let b = factories::team(
        pool,
        TeamOptions {
            league: Some(a.league.clone()),
            name: Some("Turf Titans".into()),
            ..Default::default()
        },
    )
    .await;

    let contest = factories::contest_with_games(pool, "Week 1", &[SLATE_GAME]).await;
    let entry_id = factories::full_entry(pool, contest, a.team.id, rb1).await;

    let fillers: Vec<_> = (1..=7)
        .map(|i| factories::player(&format!("00-F{i}"), &format!("Filler Player{i}")))
        .collect();
    app.nfl.seed_for_test(&fillers, scheduled).await.unwrap();

    (a, b, entry_id)
}

/// `setup_entry` with the schedule loaded and "Alpha Runner" at RB1: 100
/// rushing yards (10.00) plus the 100+ RuY Gm bonus (3.00) = 13.00. QB
/// "Filler Player1" throws for 200 yards = 8.00. The page total of 21.00
/// matches no single row, so a total assertion cannot pass on a row instead.
async fn setup(pool: &SqlitePool, app: &TestApp) -> (GeneratedTeam, GeneratedTeam, i64) {
    let teams = setup_entry(pool, app, "00-A", &[scheduled_game()]).await;
    app.nfl
        .seed_for_test(&[factories::player("00-A", "Alpha Runner")], &[])
        .await
        .unwrap();
    let buf = NflTeamAbbr("BUF".into());
    let kc = NflTeamAbbr("KC".into());
    let rb = NflPlayerWeekStats {
        opponent: Some(buf.clone()),
        ..factories::rb_stats("00-A", 1, 100)
    };
    let qb = NflPlayerWeekStats {
        passing_yards: 200,
        opponent: Some(buf.clone()),
        ..factories::player_stats("00-F1", 1)
    };
    let buf_player = NflPlayerWeekStats {
        gsis_id: "00-BUF".into(),
        team: buf.clone(),
        opponent: Some(kc.clone()),
        ..factories::player_stats("00-BUF", 1)
    };
    let buf_defense = NflTeamWeekStats {
        gsis_game_id: SLATE_GAME.into(),
        ..factories::scoreless_defense_stats("BUF", "KC", 1)
    };
    app.nfl
        .seed_week_stats_for_test(
            Season(2025),
            &[rb, qb, buf_player],
            &[
                factories::scoreless_defense_stats("KC", "BUF", 1),
                buf_defense,
            ],
        )
        .await
        .unwrap();
    teams
}

pub(crate) async fn setup_staggered_entry(
    pool: &SqlitePool,
    app: &TestApp,
) -> (GeneratedTeam, i64, Vec<Game>) {
    let owner = factories::team(
        pool,
        TeamOptions {
            name: Some("Staggered Stars".into()),
            ..Default::default()
        },
    )
    .await;

    let early = factories::game_at(SLATE_GAME, 1, datetime!(2025-09-07 17:00 UTC));
    let mut finished = factories::game_at("2025_01_LA_SF", 1, datetime!(2025-09-07 16:00 UTC));
    finished.home_team = NflTeamAbbr("SF".into());
    finished.away_team = NflTeamAbbr("LA".into());
    let mut later = factories::game_at("2025_01_DAL_PHI", 1, datetime!(2025-09-07 20:25 UTC));
    later.home_team = NflTeamAbbr("PHI".into());
    later.away_team = NflTeamAbbr("DAL".into());
    let games = vec![early, finished, later];
    let game_ids: Vec<&str> = games
        .iter()
        .map(|game| game.gsis_game_id.as_str())
        .collect();
    let contest = factories::contest_with_games(pool, "Week 1", &game_ids).await;
    let entry_id = factories::full_entry(pool, contest, owner.team.id, "00-A").await;

    let mut alpha = factories::player("00-A", "Alpha Runner");
    alpha.espn_id = Some("live-alpha".into());
    alpha.latest_team = Some(NflTeamAbbr("KC".into()));
    let mut later_qb = factories::player("00-F1", "Later Quarterback");
    later_qb.espn_id = Some("live-later-qb".into());
    later_qb.latest_team = Some(NflTeamAbbr("PHI".into()));
    let mut finished_rb = factories::player("00-F2", "Finished Runner");
    finished_rb.espn_id = Some("live-finished-rb".into());
    finished_rb.latest_team = Some(NflTeamAbbr("SF".into()));
    let fillers: Vec<_> = (3..=7)
        .map(|i| factories::player(&format!("00-F{i}"), &format!("Filler Player{i}")))
        .collect();
    let mut players = vec![alpha, later_qb, finished_rb];
    players.extend(fillers);
    app.nfl.seed_for_test(&players, &games).await.unwrap();

    (owner, entry_id, games)
}

async fn link_local_pick_media(app: &TestApp) -> String {
    let media_id = app
        .state
        .db
        .write_tx(async |conn| -> Result<_, crate::AppError> {
            let player = player_store::upsert(&mut *conn, "00-A", "00-A").await?;
            let media_id = media::save_png(
                &mut *conn,
                &app.state.config.media_dir,
                None,
                None,
                &png_bytes(2, 2),
                2,
                2,
            )
            .await?;
            player_store::set_headshot_media_id(&mut *conn, player.id, media_id).await?;
            team_store::set_logo_media_id(&mut *conn, "KC", media_id).await?;
            Ok(media_id)
        })
        .await
        .expect("link local pick media");

    media_store::get(&app.pool, media_id)
        .await
        .expect("load local pick media")
        .expect("local pick media row")
        .url()
}

fn pick_shot_sources(body: &str) -> Vec<&str> {
    const IMG_SRC_PREFIX: &str = r#"<img src=""#;

    body.split(r#"<div class="pick-shot"#)
        .skip(1)
        .map(|block| {
            let image_start = block.find(IMG_SRC_PREFIX).expect("pick-shot image");
            let source = &block[image_start + IMG_SRC_PREFIX.len()..];
            source.split_once('"').expect("pick-shot image source").0
        })
        .collect()
}

fn assert_pick_shot_sources(body: &str, local_url: &str) {
    let sources = pick_shot_sources(body);
    assert_eq!(sources.len(), 9, "entry should render nine pick images");
    assert_eq!(sources[1], local_url, "RB1 uses local player media");
    assert_eq!(sources[8], local_url, "D/ST uses local team media");
    assert_eq!(
        sources
            .iter()
            .filter(|&&source| source == local_url)
            .count(),
        2,
        "two picks use local media"
    );
    assert_eq!(
        sources
            .iter()
            .filter(|&&source| source.starts_with(crate::entries::service::SILHOUETTE))
            .count(),
        7,
        "seven picks use silhouettes"
    );
    assert!(
        sources.iter().all(|&source| {
            source == local_url || source.starts_with(crate::entries::service::SILHOUETTE)
        }),
        "every pick image must be local media or a silhouette: {sources:?}"
    );
}

#[sqlx::test]
async fn member_after_kickoff_sees_scored_lineup(pool: SqlitePool) {
    let app = TestApp::from_pool_at(pool.clone(), KICKOFF).await;
    let (a, b, entry_id) = setup(&pool, &app).await;

    app.login_as(&b.owner).await;
    let resp = app.get(&format!("/entries/{entry_id}")).await;
    resp.assert_status_ok();
    let body = resp.text();
    // The bar carries the page title; the team name sits in the summary band.
    assert!(body.contains("Lineup Scores"));
    assert!(body.contains(&as_rendered(&a.team.name)));
    assert!(body.contains("Alpha Runner"));
    // The scoring line is in the expand panel, the box score on the row.
    assert!(body.contains("100 RuY"));
    assert!(
        body.contains("100 YDS"),
        "RB collapsed line is the box score"
    );
    // RB1: 100 rushing yards + the 100-yard bonus = 13.00, split for display.
    assert!(
        body.contains(
            r#"<span class="pick-pts-text"><span class="pick-pts-whole">13</span><span class="pick-pts-dec">.00</span></span>"#
        ),
        "RB1 row points render"
    );
    assert!(body.contains("Filler Player1"));
    assert!(body.contains("200 PaY"));
    assert!(
        body.contains(
            r#"<span class="pick-pts-text"><span class="pick-pts-whole">8</span><span class="pick-pts-dec">.00</span></span>"#
        ),
        "QB row points render"
    );
    assert!(body.contains(">21.00<"), "page total renders");
}

#[sqlx::test]
async fn staggered_lineup_scores(pool: SqlitePool) {
    let now = datetime!(2025-09-07 18:30 UTC);
    let app = TestApp::from_pool_at(pool.clone(), now).await;
    let (owner, entry_id, games) = setup_staggered_entry(&pool, &app).await;
    let contest_id: i64 = sqlx::query_scalar("SELECT contest_id FROM entries WHERE id = ?1")
        .bind(entry_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    let contest = crate::contests::ContestId(contest_id);
    let identity = crate::live::identity::LiveIdentityIndex::from_sources(
        &app.nfl.players().await.unwrap(),
        &[],
    );
    app.state
        .live
        .register_contest(contest, games.clone(), Arc::new(identity), now);

    let early_live = live_game(&games[0]).expect("fixture kickoff");
    app.state.live.publish_event(
        contest,
        &box_score_event(
            &games[0],
            1,
            LiveGamePhase::InProgress,
            now,
            "2",
            "08:00",
            vec![live_player_stats(
                "live-alpha",
                "Alpha Runner",
                "KC",
                "BUF",
                "RB",
                100,
            )],
        ),
        std::slice::from_ref(&early_live),
    );
    let finished_live = live_game(&games[1]).expect("fixture kickoff");
    app.state.live.publish_event(
        contest,
        &box_score_event(
            &games[1],
            2,
            LiveGamePhase::Final,
            now,
            "4",
            "00:00",
            vec![live_player_stats(
                "live-finished-rb",
                "Finished Runner",
                "SF",
                "LA",
                "RB",
                100,
            )],
        ),
        std::slice::from_ref(&finished_live),
    );

    app.login_as(&owner.owner).await;
    let body = app.get(&format!("/entries/{entry_id}")).await.text();
    assert!(
        body.contains(&format!(
            r#"<div id="entry-{entry_id}-rb1-points" class="pick-pts text-base-content"><span class="pick-pts-text"><span class="pick-pts-whole">13</span><span class="pick-pts-dec">.00</span></span><span class="pick-pts-delta"></span></div>"#
        )),
        "in-progress numeric row rests black with its live score: {body}"
    );
    assert!(
        body.contains(&format!(
            r#"<div id="entry-{entry_id}-rb2-points" class="pick-pts text-base-content"><span class="pick-pts-text"><span class="pick-pts-whole">13</span><span class="pick-pts-dec">.00</span></span><span class="pick-pts-delta"></span></div>"#
        )),
        "final numeric row is settled black with its earned score: {body}"
    );
    assert!(
        body.contains(&format!(
            r#"<div id="entry-{entry_id}-qb-status" class="mt-0.5 text-[11px] font-medium text-base-content/50">Sun 9/7 4:25pm</div>"#
        )),
        "scheduled row's status carries the compact Eastern kickoff: {body}"
    );
    assert!(
        body.contains(&format!(
            r#"<div id="entry-{entry_id}-qb-points" class="pick-pts text-base-content/50"><span class="pick-pts-text"><span class="pick-pts-whole"></span><span class="pick-pts-dec"></span></span><span class="pick-pts-delta"></span></div>"#
        )),
        "scheduled row leaves the score cell empty: {body}"
    );
    assert!(
        body.contains(&format!(
            r#"<div id="entry-{entry_id}-qb-stats" class="mt-0.5 text-[13px] text-base-content/50"></div>"#
        )),
        "scheduled row leaves the stats cell empty: {body}"
    );
    assert!(
        body.contains(r#"<div class="text-2xl font-bold tnum">26.00</div>"#),
        "scheduled zero still sums into the lineup total: {body}"
    );
}

/// The entry's contest is the only contest, live at the pinned clock — the
/// Current Contest — so the Current tab lights.
#[sqlx::test]
async fn entry_in_the_current_contest_lights_the_current_tab(pool: SqlitePool) {
    let app = TestApp::from_pool_at(pool.clone(), KICKOFF).await;
    let (_a, b, entry_id) = setup(&pool, &app).await;

    app.login_as(&b.owner).await;
    let resp = app.get(&format!("/entries/{entry_id}")).await;
    resp.assert_status_ok();
    let body = resp.text();
    assert_eq!(
        body.matches(ACTIVE).count(),
        1,
        "exactly one tab should be active: {body}"
    );
    assert_eq!(
        href_before(&body, ACTIVE),
        "/",
        "the entry's contest is Current, so the Current tab should light: {body}"
    );
    assert!(
        href_before(&body, "ph-caret-left").starts_with("/contests/"),
        "the bar links back to the lineup's contest: {body}"
    );

    // Current anchors the center of the tab bar, its disc filled while active.
    let tab_bar = body
        .split_once(r#"aria-label="Primary""#)
        .map(|(_, rest)| rest)
        .expect("tab bar renders");
    let idx = |label: &str| {
        tab_bar
            .find(label)
            .unwrap_or_else(|| panic!("{label} in the tab bar: {body}"))
    };
    assert!(
        idx("Standings") < idx("Contests")
            && idx("Contests") < idx("Current")
            && idx("Current") < idx("My Team")
            && idx("My Team") < idx("More"),
        "Current sits center among the five tabs: {body}"
    );
    assert!(
        tab_bar.contains("rounded-full bg-primary"),
        "the active Current disc fills green: {body}"
    );
}

/// A second, sooner-kicking-off contest steals Current from the entry's
/// contest, so the entry page lights Contests instead.
#[sqlx::test]
async fn entry_in_a_non_current_contest_lights_the_contests_tab(pool: SqlitePool) {
    let now = datetime!(2025-09-01 00:00 UTC);
    let app = TestApp::from_pool_at(pool.clone(), now).await;
    let (_a, b, entry_id) = setup_entry(&pool, &app, "00-A", &[scheduled_game()]).await;

    // Week 2 kicks off sooner than Week 1 (the entry's contest), so it — not
    // Week 1 — is the Current Contest.
    factories::contest_with_games(&pool, "Week 2", &["2025_02_BUF_KC"]).await;
    let g2 = factories::game_at("2025_02_BUF_KC", 2, KICKOFF - time::Duration::days(2));
    app.nfl.seed_for_test(&[], &[g2]).await.unwrap();

    app.login_as(&b.owner).await;
    let resp = app.get(&format!("/entries/{entry_id}")).await;
    resp.assert_status_ok();
    let body = resp.text();
    assert_eq!(
        body.matches(ACTIVE).count(),
        1,
        "exactly one tab should be active: {body}"
    );
    assert_eq!(
        href_before(&body, ACTIVE),
        "/contests",
        "a non-current contest's entry should light Contests, not Current: {body}"
    );
    assert!(
        href_before(&body, "ph-caret-left").starts_with("/contests/"),
        "the bar links back to the lineup's contest: {body}"
    );

    // Inactive, the Current disc stays an outlined circle, not a green fill.
    let tab_bar = body
        .split_once(r#"aria-label="Primary""#)
        .map(|(_, rest)| rest)
        .expect("tab bar renders");
    assert!(
        tab_bar.contains("border border-base-content/25"),
        "the inactive Current disc is outlined: {body}"
    );
    assert!(
        !tab_bar.contains("rounded-full bg-primary"),
        "the inactive Current disc must not fill green: {body}"
    );
}

#[sqlx::test]
async fn unknown_entry_is_404(pool: SqlitePool) {
    let app = TestApp::from_pool(pool.clone()).await;
    let (a, _b, _entry_id) = setup(&pool, &app).await;

    app.login_as(&a.owner).await;
    let resp = app.get("/entries/999999").await;
    resp.assert_status_not_found();
}

#[sqlx::test]
async fn user_outside_the_league_gets_404(pool: SqlitePool) {
    let app = TestApp::from_pool(pool.clone()).await;
    let (_a, _b, entry_id) = setup(&pool, &app).await;
    // A fantasy team in a different league (the factory default creates one).
    let outsider = factories::team(&pool, TeamOptions::default()).await;

    app.login_as(&outsider.owner).await;
    let resp = app.get(&format!("/entries/{entry_id}")).await;
    resp.assert_status_not_found();
}

#[sqlx::test]
async fn unauthenticated_request_is_redirected_to_login(pool: SqlitePool) {
    let app = TestApp::from_pool(pool.clone()).await;
    let (_a, _b, entry_id) = setup(&pool, &app).await;

    // No login_as call: this request carries no session cookie.
    let resp = app.get(&format!("/entries/{entry_id}")).await;
    assert_eq!(resp.status_code(), StatusCode::TEMPORARY_REDIRECT);
}

#[sqlx::test]
async fn slate_missing_from_the_schedule_is_404(pool: SqlitePool) {
    // nfl-data holds no schedule for the season — an unsynced cache, not a
    // state the app database can rule out. The page must not panic.
    let app = TestApp::from_pool_at(pool.clone(), KICKOFF).await;
    let (a, _b, entry_id) = setup_entry(&pool, &app, "00-A", &[]).await;

    app.login_as(&a.owner).await;
    let resp = app.get(&format!("/entries/{entry_id}")).await;
    resp.assert_status_not_found();
}

#[sqlx::test]
async fn unknown_kickoff_hides_picks_from_non_owners(pool: SqlitePool) {
    // A slate game with no kickoff time yet. Nobody can say it started, so
    // the lineup stays private.
    let app = TestApp::from_pool_at(pool.clone(), KICKOFF).await;
    let (a, b, entry_id) =
        setup_entry(&pool, &app, "00-A", &[factories::game(SLATE_GAME, 1)]).await;

    app.login_as(&b.owner).await;
    let resp = app.get(&format!("/entries/{entry_id}")).await;
    resp.assert_status_ok();
    assert!(resp.text().contains("Picks will be shown after kickoff"));

    app.login_as(&a.owner).await;
    let resp = app.get(&format!("/entries/{entry_id}")).await;
    resp.assert_status_ok();
    assert!(
        resp.text().contains("Filler Player1"),
        "the owner still sees"
    );
}

#[sqlx::test]
async fn owner_before_kickoff_sees_full_lineup(pool: SqlitePool) {
    let app = TestApp::from_pool_at(pool.clone(), BEFORE_KICKOFF).await;
    let (a, _b, entry_id) = setup(&pool, &app).await;

    app.login_as(&a.owner).await;
    let resp = app.get(&format!("/entries/{entry_id}")).await;
    resp.assert_status_ok();
    let body = resp.text();
    assert!(body.contains("Alpha Runner"));
    assert!(!body.contains("Picks will be shown after kickoff"));
    assert!(
        body.contains(">Edit</a>") && body.contains("/draft-entry"),
        "the owner edits from the lineup page while the slate is open: {body}"
    );
    assert!(
        !body.contains("pick-pts"),
        "nothing scores before kickoff: {body}"
    );
}

#[sqlx::test]
async fn non_owner_before_kickoff_sees_note_not_picks(pool: SqlitePool) {
    let app = TestApp::from_pool_at(pool.clone(), BEFORE_KICKOFF).await;
    let (a, b, entry_id) = setup(&pool, &app).await;

    app.login_as(&b.owner).await;
    let resp = app.get(&format!("/entries/{entry_id}")).await;
    resp.assert_status_ok();
    let body = resp.text();
    assert!(
        body.contains(&as_rendered(&a.team.name)),
        "header still shows the team"
    );
    assert!(body.contains("Picks will be shown after kickoff"));
    assert!(!body.contains("Alpha Runner"), "no player names leak");
    assert!(!body.contains("Filler Player1"), "no player names leak");
    assert!(
        !body.contains("/draft-entry"),
        "only the owner may edit: {body}"
    );
}

#[sqlx::test]
async fn roster_only_pick_is_named_from_the_weekly_roster(pool: SqlitePool) {
    // "00-R" exists only as a weekly roster entry, like the 144 rostered
    // players absent from the players release.
    let app = TestApp::from_pool_at(pool.clone(), KICKOFF).await;
    let (a, _b, entry_id) = setup_entry(&pool, &app, "00-R", &[scheduled_game()]).await;
    app.nfl
        .seed_weekly_roster_for_test(&[factories::weekly_roster_entry(
            "00-R",
            "Rostered Runner",
            1,
        )])
        .await
        .unwrap();

    app.login_as(&a.owner).await;
    let resp = app.get(&format!("/entries/{entry_id}")).await;
    resp.assert_status_ok();
    let body = resp.text();
    assert!(body.contains("Rostered Runner"));
    assert!(
        !body.contains(">00-R<"),
        "raw id must not reach the name cell"
    );
    assert!(
        !body.contains("/draft-entry"),
        "no editing once the slate kicks off: {body}"
    );
}

#[sqlx::test]
async fn expanded_row_shows_salary_ownership_and_breakdown(pool: SqlitePool) {
    let app = TestApp::from_pool_at(pool.clone(), KICKOFF).await;
    let (a, _b, entry_id) = setup(&pool, &app).await;
    sqlx::query(
        "INSERT INTO nfl_player_salaries (gsis_game_id, gsis_player_id, team_abbr, dfs_position, salary)
         VALUES (?1, '00-A', 'KC', 'RB', 5700)",
    ).bind(SLATE_GAME).execute(&pool).await.unwrap();

    app.login_as(&a.owner).await;
    let body = app.get(&format!("/entries/{entry_id}")).await.text();

    assert!(
        body.contains(r#"<details name="pick""#),
        "rows are exclusive-accordion details"
    );
    assert!(body.contains("$5,700"), "salary renders");
    assert!(body.contains("100.0%"), "ownership renders");
    // RB1's breakdown: the per-stat line and the 100-yard bonus.
    assert!(body.contains("Score breakdown"));
    assert!(body.contains("100 RuY"));
    assert!(body.contains("100+ RuY Gm"));
}

#[sqlx::test]
async fn entry_images_use_local_media_and_placeholders_after_kickoff(pool: SqlitePool) {
    let app = TestApp::from_pool_at(pool.clone(), KICKOFF).await;
    let (a, _b, entry_id) = setup(&pool, &app).await;
    let local_url = link_local_pick_media(&app).await;

    app.login_as(&a.owner).await;
    let resp = app.get(&format!("/entries/{entry_id}")).await;
    resp.assert_status_ok();
    assert_pick_shot_sources(&resp.text(), &local_url);
    app.get(&local_url).await.assert_status_ok();
}

#[sqlx::test]
async fn entry_images_use_local_media_and_placeholders_before_kickoff(pool: SqlitePool) {
    let app = TestApp::from_pool_at(pool.clone(), BEFORE_KICKOFF).await;
    let (a, _b, entry_id) = setup(&pool, &app).await;
    let local_url = link_local_pick_media(&app).await;

    app.login_as(&a.owner).await;
    let resp = app.get(&format!("/entries/{entry_id}")).await;
    resp.assert_status_ok();
    assert_pick_shot_sources(&resp.text(), &local_url);
    app.get(&local_url).await.assert_status_ok();
}

#[sqlx::test]
async fn rank_badge_shows_after_results(pool: SqlitePool) {
    let app = TestApp::from_pool_at(pool.clone(), KICKOFF).await;
    let (a, _b, entry_id) = setup(&pool, &app).await;

    app.login_as(&a.owner).await;
    let body = app.get(&format!("/entries/{entry_id}")).await.text();
    // One entry scored above zero, so it ranks first.
    assert!(
        body.contains(r#"<div class="w-6 text-center text-xl font-bold tabular-nums">1</div>"#),
        "rank badge shows after results: {body}"
    );
}

#[sqlx::test]
async fn rank_badge_hidden_before_results(pool: SqlitePool) {
    // Rank and picks come from one branch, so the only viewer without a rank
    // is the one without picks: a non-owner before kickoff.
    let app = TestApp::from_pool_at(pool.clone(), BEFORE_KICKOFF).await;
    let (_a, b, entry_id) = setup(&pool, &app).await;

    app.login_as(&b.owner).await;
    let body = app.get(&format!("/entries/{entry_id}")).await.text();
    assert!(
        !body.contains(r#"class="w-6 text-center text-xl font-bold tabular-nums""#),
        "no rank badge before results"
    );
}

#[sqlx::test]
async fn game_score_line_bolds_the_picks_team(pool: SqlitePool) {
    // SLATE_GAME is KC (home) vs BUF (away) with no score set, so the pick's
    // (home) side renders bolded and neither team shows a score.
    let app = TestApp::from_pool_at(pool.clone(), KICKOFF).await;
    let (_a, b, entry_id) = setup(&pool, &app).await;

    app.login_as(&b.owner).await;
    let resp = app.get(&format!("/entries/{entry_id}")).await;
    resp.assert_status_ok();
    let body = resp.text();
    assert!(
        body.contains(r#"BUF @ <b class="font-bold text-base-content">KC</b>"#),
        "game line bolds the pick's (home) team with no score shown: {body}"
    );
}

#[sqlx::test]
async fn contest_entry_link_reaches_lineup_page(pool: SqlitePool) {
    let app = TestApp::from_pool_at(pool.clone(), KICKOFF).await;
    let (_owner, member, entry_id) = setup(&pool, &app).await;
    let contest_id: i64 = sqlx::query_scalar("SELECT contest_id FROM entries WHERE id = ?1")
        .bind(entry_id)
        .fetch_one(&pool)
        .await
        .unwrap();

    app.login_as(&member.owner).await;
    let contest = app.get(&format!("/contests/{contest_id}")).await;
    contest.assert_status_ok();
    let body = contest.text();
    let entry_path = format!("/entries/{entry_id}");
    let marker = format!(r#"href="{entry_path}""#);
    let marker_start = body.find(&marker).expect("contest links to entry");
    let href_start = marker_start + "href=\"".len();
    let rendered_path = &body[href_start..href_start + entry_path.len()];

    let entry = app.get(rendered_path).await;
    entry.assert_status_ok();
    assert!(entry.text().contains("Lineup Scores"));
}

#[sqlx::test]
async fn entry_events_reject_users_outside_the_entry_league(pool: SqlitePool) {
    let app = TestApp::from_pool_at(pool.clone(), KICKOFF).await;
    let (_owner, _member, entry_id) = setup(&pool, &app).await;
    let outsider = factories::team(&pool, TeamOptions::default()).await;

    app.login_as(&outsider.owner).await;
    let response = app.get(&format!("/entries/{entry_id}/events")).await;
    response.assert_status_not_found();
}

#[sqlx::test]
async fn unavailable_live_entry_exposes_hx_sse_connection(pool: SqlitePool) {
    let app = TestApp::from_pool_at(pool.clone(), KICKOFF).await;
    let (_owner, member, entry_id) = setup_entry(&pool, &app, "00-A", &[scheduled_game()]).await;

    app.login_as(&member.owner).await;
    let body = app.get(&format!("/entries/{entry_id}")).await.text();
    assert!(body.contains(&format!(r#"id="entry-{entry_id}-score-summary""#)));
    assert!(
        body.contains(&format!(r#"id="entry-{entry_id}-live-indicator""#)),
        "entry navbar indicator renders: {body}"
    );
    assert!(
        body.contains(r#"class="size-2 rounded-full bg-warning hidden""#),
        "unavailable stream starts with a hidden warning indicator: {body}"
    );
    assert!(body.contains("hx-on::after:sse:connection"), "{body}");
    assert!(body.contains("hx-on::sse:error"), "{body}");
    assert!(body.contains("hx-on::sse:close"), "{body}");
    assert!(body.contains(&format!(r#"hx-sse:connect="/entries/{entry_id}/events""#)));
    assert!(body.contains(r#"hx-swap="innerHTML""#));
}

#[sqlx::test]
async fn entry_events_stream_fragment_and_close_on_shutdown(pool: SqlitePool) {
    let app = TestApp::from_pool_at(pool.clone(), KICKOFF).await;
    let (_owner, member, entry_id) = setup_entry(&pool, &app, "00-A", &[scheduled_game()]).await;
    let contest_id: i64 = sqlx::query_scalar("SELECT contest_id FROM entries WHERE id = ?1")
        .bind(entry_id)
        .fetch_one(&pool)
        .await
        .unwrap();

    let cookie = app.login_as(&member.owner).await;
    let cookie = cookie.split(';').next().unwrap().to_owned();
    let request = Request::builder()
        .uri(format!("/entries/{entry_id}/events"))
        .header("cookie", cookie)
        .body(Body::empty())
        .unwrap();
    let response = app.router.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap()
            .split(';')
            .next(),
        Some("text/event-stream")
    );

    let body = response.into_body();
    let (body, _) = tokio::join!(
        tokio::time::timeout(std::time::Duration::from_secs(5), body.collect()),
        async {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            app.state
                .live
                .notify(crate::contests::ContestId(contest_id));
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            app.state.live.shutdown();
        }
    );
    let body = body.expect("entry SSE stream did not close").unwrap();
    let body = String::from_utf8(body.to_bytes().to_vec()).unwrap();

    assert!(
        body.matches("data: ").count() >= 2,
        "initial and update frames: {body}"
    );
    assert!(body.contains(&format!("id=\"entry-{entry_id}-live-indicator\"")));
    assert!(body.contains(&format!(
        "hx-swap-oob=\"outerHTML:#entry-{entry_id}-live-indicator\""
    )));
    assert!(body.contains(&format!(
        "data: <div hx-swap-oob=\"innerHTML:#entry-{entry_id}-score-summary\">"
    )));
    assert!(!body.contains(": shutdown"));
    assert!(!body.contains("event: message"));
}

#[sqlx::test]
async fn entry_sse_score_direction(pool: SqlitePool) {
    let app = TestApp::from_pool_at(pool.clone(), KICKOFF).await;
    let (_owner, member, entry_id) = setup_entry(&pool, &app, "00-A", &[scheduled_game()]).await;
    let contest_id: i64 = sqlx::query_scalar("SELECT contest_id FROM entries WHERE id = ?1")
        .bind(entry_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    let contest = crate::contests::ContestId(contest_id);
    let game = scheduled_game();
    let live_game = live_game(&game).expect("fixture kickoff");
    let mut alpha = factories::player("00-A", "Alpha Runner");
    alpha.espn_id = Some("live-alpha".into());
    app.nfl.seed_for_test(&[alpha], &[]).await.unwrap();
    let identity = crate::live::identity::LiveIdentityIndex::from_sources(
        &app.nfl.players().await.unwrap(),
        &[],
    );
    app.state
        .live
        .register_contest(contest, vec![game.clone()], Arc::new(identity), KICKOFF);

    let observed_at = KICKOFF + time::Duration::seconds(30);
    app.state.live.publish_event(
        contest,
        &box_score_event(
            &game,
            1,
            LiveGamePhase::InProgress,
            observed_at,
            "2",
            "08:00",
            vec![live_player_stats(
                "live-alpha",
                "Alpha Runner",
                "KC",
                "BUF",
                "RB",
                100,
            )],
        ),
        std::slice::from_ref(&live_game),
    );

    let cookie = app.login_as(&member.owner).await;
    let cookie = cookie.split(';').next().unwrap().to_owned();
    let request = Request::builder()
        .uri(format!("/entries/{entry_id}/events"))
        .header("cookie", cookie)
        .body(Body::empty())
        .unwrap();
    let response = app.router.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let mut stream = SseReader::new(response.into_body());
    let summary_start_marker =
        format!(r#"<div hx-swap-oob="innerHTML:#entry-{entry_id}-score-summary">"#);
    let regions_start_marker =
        format!(r#"<div hx-swap-oob="outerHTML:#entry-{entry_id}-qb-game">"#);
    let assert_summary = |event: &str, expected_total: &str| {
        let summary_start = event
            .find(&summary_start_marker)
            .expect("summary OOB updates");
        let regions_start = event.find(&regions_start_marker).expect("row OOB updates");
        let summary = &event[summary_start..regions_start];
        assert!(
            !summary.contains("score-flash-increase") && !summary.contains("score-flash-decrease"),
            "summary never flashes: {summary}"
        );
        assert!(
            summary.contains(&format!(
                r#"<div class="text-2xl font-bold tnum">{expected_total}</div>"#
            )),
            "summary carries the updated total: {summary}"
        );
    };

    let initial = tokio::time::timeout(std::time::Duration::from_secs(5), stream.next_event())
        .await
        .expect("entry SSE initial event timed out")
        .expect("entry SSE closed before initial event");
    assert!(
        initial.contains(&format!(
            r#"<div id="entry-{entry_id}-rb1-points" class="pick-pts text-base-content"><span class="pick-pts-text"><span class="pick-pts-whole">13</span><span class="pick-pts-dec">.00</span></span><span class="pick-pts-delta"></span></div>"#
        )),
        "initial frame carries no direction class on the live RB1 row: {initial}"
    );
    assert_summary(&initial, "13.00");

    app.state.live.publish_event(
        contest,
        &box_score_event(
            &game,
            2,
            LiveGamePhase::InProgress,
            observed_at + time::Duration::seconds(30),
            "2",
            "08:00",
            vec![live_player_stats(
                "live-alpha",
                "Alpha Runner",
                "KC",
                "BUF",
                "RB",
                120,
            )],
        ),
        std::slice::from_ref(&live_game),
    );
    let increase = tokio::time::timeout(std::time::Duration::from_secs(5), stream.next_event())
        .await
        .expect("entry SSE increase event timed out")
        .expect("entry SSE closed before increase event");
    assert!(
        increase.contains(&format!(
            r#"<div id="entry-{entry_id}-rb1-points" class="pick-pts text-base-content"><span class="pick-pts-text score-flash-increase"><span class="pick-pts-whole">15</span><span class="pick-pts-dec">.00</span></span><span class="pick-pts-delta score-flash-increase">+2.00</span></div>"#
        )),
        "increase flashes in place on the changed live row: {increase}"
    );
    assert_summary(&increase, "15.00");

    app.state.live.publish_event(
        contest,
        &box_score_event(
            &game,
            3,
            LiveGamePhase::InProgress,
            observed_at + time::Duration::seconds(60),
            "2",
            "08:00",
            vec![live_player_stats(
                "live-alpha",
                "Alpha Runner",
                "KC",
                "BUF",
                "RB",
                120,
            )],
        ),
        std::slice::from_ref(&live_game),
    );
    let unchanged = tokio::time::timeout(std::time::Duration::from_secs(5), stream.next_event())
        .await
        .expect("entry SSE unchanged event timed out")
        .expect("entry SSE closed before unchanged event");
    assert!(
        unchanged.contains(&format!(
            r#"<div id="entry-{entry_id}-rb1-points" class="pick-pts text-base-content"><span class="pick-pts-text"><span class="pick-pts-whole">15</span><span class="pick-pts-dec">.00</span></span><span class="pick-pts-delta"></span></div>"#
        )),
        "an unchanged displayed score emits no direction class: {unchanged}"
    );
    assert_summary(&unchanged, "15.00");

    app.state.live.publish_event(
        contest,
        &box_score_event(
            &game,
            4,
            LiveGamePhase::InProgress,
            observed_at + time::Duration::seconds(90),
            "2",
            "08:00",
            vec![live_player_stats(
                "live-alpha",
                "Alpha Runner",
                "KC",
                "BUF",
                "RB",
                100,
            )],
        ),
        std::slice::from_ref(&live_game),
    );
    let decrease = tokio::time::timeout(std::time::Duration::from_secs(5), stream.next_event())
        .await
        .expect("entry SSE decrease event timed out")
        .expect("entry SSE closed before decrease event");
    assert!(
        decrease.contains(&format!(
            r#"<div id="entry-{entry_id}-rb1-points" class="pick-pts text-base-content"><span class="pick-pts-text score-flash-decrease"><span class="pick-pts-whole">13</span><span class="pick-pts-dec">.00</span></span><span class="pick-pts-delta score-flash-decrease">-2.00</span></div>"#
        )),
        "a correction flashes in place on the changed live row: {decrease}"
    );
    assert_summary(&decrease, "13.00");

    app.state.live.publish_event(
        contest,
        &box_score_event(
            &game,
            5,
            LiveGamePhase::InProgress,
            observed_at + time::Duration::seconds(120),
            "2",
            "08:00",
            vec![unknown_live_player_stats()],
        ),
        std::slice::from_ref(&live_game),
    );
    let missing = tokio::time::timeout(std::time::Duration::from_secs(5), stream.next_event())
        .await
        .expect("entry SSE missing-stat event timed out")
        .expect("entry SSE closed before missing-stat event");
    assert!(
        missing.contains(&format!(
            r#"<div id="entry-{entry_id}-rb1-points" class="pick-pts text-base-content/50"><span class="pick-pts-text"><span class="pick-pts-whole"></span><span class="pick-pts-dec"></span></span><span class="pick-pts-delta"></span></div>"#
        )),
        "unavailable row blanks the score with no direction class: {missing}"
    );
    assert!(!missing.contains("score-flash-increase"));
    assert!(!missing.contains("score-flash-decrease"));

    app.state.live.publish_event(
        contest,
        &box_score_event(
            &game,
            6,
            LiveGamePhase::InProgress,
            observed_at + time::Duration::seconds(150),
            "2",
            "08:00",
            vec![live_player_stats(
                "live-alpha",
                "Alpha Runner",
                "KC",
                "BUF",
                "RB",
                110,
            )],
        ),
        std::slice::from_ref(&live_game),
    );
    let recovery = tokio::time::timeout(std::time::Duration::from_secs(5), stream.next_event())
        .await
        .expect("entry SSE recovery event timed out")
        .expect("entry SSE closed before recovery event");
    assert!(
        recovery.contains(&format!(
            r#"<div id="entry-{entry_id}-rb1-points" class="pick-pts text-base-content"><span class="pick-pts-text"><span class="pick-pts-whole">14</span><span class="pick-pts-dec">.00</span></span><span class="pick-pts-delta"></span></div>"#
        )),
        "a recovered score re-baselines without flashing: {recovery}"
    );
    assert_summary(&recovery, "14.00");
}

#[sqlx::test]
async fn entry_sse_injury_badge(pool: SqlitePool) {
    let app = TestApp::from_pool_at(pool.clone(), KICKOFF).await;
    let (_owner, member, entry_id) = setup_entry(&pool, &app, "00-A", &[scheduled_game()]).await;
    let contest_id: i64 = sqlx::query_scalar("SELECT contest_id FROM entries WHERE id = ?1")
        .bind(entry_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    let contest = crate::contests::ContestId(contest_id);
    let game = scheduled_game();
    let live_game = live_game(&game).expect("fixture kickoff");
    let mut alpha = factories::player("00-A", "Alpha Runner");
    alpha.espn_id = Some("live-alpha".into());
    app.nfl.seed_for_test(&[alpha], &[]).await.unwrap();
    let identity = crate::live::identity::LiveIdentityIndex::from_sources(
        &app.nfl.players().await.unwrap(),
        &[],
    );
    app.state
        .live
        .register_contest(contest, vec![game.clone()], Arc::new(identity), KICKOFF);

    // The coordinator's merged designation map, published for the slate tuple.
    app.state.injuries.publish(
        (Season(2025), nfl_data::Week(1), nfl_data::SeasonType::Reg),
        crate::injuries::SlateInjuries {
            by_gsis: std::collections::HashMap::from([(
                crate::entries::NflPlayerId("00-A".into()),
                crate::injuries::InjuryDesignation::Questionable,
            )]),
            report_date: None,
            attempted_at: Some(KICKOFF),
        },
    );
    app.state.live.publish_event(
        contest,
        &box_score_event(
            &game,
            1,
            LiveGamePhase::InProgress,
            KICKOFF + time::Duration::seconds(30),
            "2",
            "08:00",
            vec![live_player_stats(
                "live-alpha",
                "Alpha Runner",
                "KC",
                "BUF",
                "RB",
                100,
            )],
        ),
        std::slice::from_ref(&live_game),
    );

    let cookie = app.login_as(&member.owner).await;
    let cookie = cookie.split(';').next().unwrap().to_owned();
    let request = Request::builder()
        .uri(format!("/entries/{entry_id}/events"))
        .header("cookie", cookie)
        .body(Body::empty())
        .unwrap();
    let response = app.router.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let mut stream = SseReader::new(response.into_body());
    let initial = tokio::time::timeout(std::time::Duration::from_secs(5), stream.next_event())
        .await
        .expect("entry SSE initial event timed out")
        .expect("entry SSE closed before initial event");

    let flat = |text: &str| {
        let body = text
            .lines()
            .map(|line| line.strip_prefix("data: ").unwrap_or(line))
            .collect::<Vec<_>>()
            .join("\n");
        body.split_whitespace().collect::<String>()
    };
    let chip = flat(&format!(
        r##"<span id="entry-{entry_id}-rb1-injury" class="inline-flex shrink-0" hx-swap-oob="outerHTML:#entry-{entry_id}-rb1-injury"><span class="badge badge-warning badge-outline badge-xs" title="Questionable" aria-label="Questionable">Q</span></span>"##
    ));
    assert!(
        flat(&initial).contains(&chip),
        "the injured pick carries its chip next to the name: {initial}"
    );
    assert!(
        flat(&initial).contains(&flat(&format!(
            r#"<span id="entry-{entry_id}-qb-injury" class="inline-flex shrink-0" hx-swap-oob="outerHTML:#entry-{entry_id}-qb-injury"></span>"#
        ))),
        "a healthy pick renders an empty injury region: {initial}"
    );
    assert!(
        initial.contains(&format!(
            r#"<span id="entry-{entry_id}-rb1-injury" class="inline-flex shrink-0" hx-swap-oob="outerHTML:#entry-{entry_id}-rb1-injury">"#
        )),
        "the OOB anchor is the injury span itself, not a wrapper div"
    );
    assert!(
        !initial.contains(&format!(
            r#"<div hx-swap-oob="outerHTML:#entry-{entry_id}-rb1-injury">"#
        )),
        "no block wrapper around an inline injury span"
    );

    app.state.live.shutdown();
}

#[sqlx::test]
async fn entry_sse_suppresses_injury_badge_when_row_is_final(pool: SqlitePool) {
    let app = TestApp::from_pool_at(pool.clone(), KICKOFF).await;
    let (_owner, member, entry_id) = setup_entry(&pool, &app, "00-A", &[scheduled_game()]).await;
    let contest_id: i64 = sqlx::query_scalar("SELECT contest_id FROM entries WHERE id = ?1")
        .bind(entry_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    let contest = crate::contests::ContestId(contest_id);
    let game = scheduled_game();
    let live_game = live_game(&game).expect("fixture kickoff");
    let mut alpha = factories::player("00-A", "Alpha Runner");
    alpha.espn_id = Some("live-alpha".into());
    app.nfl.seed_for_test(&[alpha], &[]).await.unwrap();
    let identity = crate::live::identity::LiveIdentityIndex::from_sources(
        &app.nfl.players().await.unwrap(),
        &[],
    );
    app.state
        .live
        .register_contest(contest, vec![game.clone()], Arc::new(identity), KICKOFF);

    app.state.injuries.publish(
        (Season(2025), nfl_data::Week(1), nfl_data::SeasonType::Reg),
        crate::injuries::SlateInjuries {
            by_gsis: std::collections::HashMap::from([(
                crate::entries::NflPlayerId("00-A".into()),
                crate::injuries::InjuryDesignation::Questionable,
            )]),
            report_date: None,
            attempted_at: Some(KICKOFF),
        },
    );
    app.state.live.publish_event(
        contest,
        &box_score_event(
            &game,
            1,
            LiveGamePhase::Final,
            KICKOFF + time::Duration::hours(3),
            "Final",
            "",
            vec![live_player_stats(
                "live-alpha",
                "Alpha Runner",
                "KC",
                "BUF",
                "RB",
                100,
            )],
        ),
        std::slice::from_ref(&live_game),
    );

    let cookie = app.login_as(&member.owner).await;
    let cookie = cookie.split(';').next().unwrap().to_owned();
    let request = Request::builder()
        .uri(format!("/entries/{entry_id}/events"))
        .header("cookie", cookie)
        .body(Body::empty())
        .unwrap();
    let response = app.router.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let mut stream = SseReader::new(response.into_body());
    let initial = tokio::time::timeout(std::time::Duration::from_secs(5), stream.next_event())
        .await
        .expect("entry SSE initial event timed out")
        .expect("entry SSE closed before initial event");

    assert!(
        initial.contains(&format!(
            r#"<span id="entry-{entry_id}-rb1-injury" class="inline-flex shrink-0" hx-swap-oob="outerHTML:#entry-{entry_id}-rb1-injury"></span>"#
        )),
        "a finished game row suppresses the chip entirely: {initial}"
    );
    assert!(!initial.contains("badge badge-warning"));

    app.state.live.shutdown();
}

#[sqlx::test]
async fn contest_events_stream_fragment_and_close_on_shutdown(pool: SqlitePool) {
    let app = TestApp::from_pool_at(pool.clone(), KICKOFF).await;
    let (_owner, member, entry_id) = setup_entry(&pool, &app, "00-A", &[scheduled_game()]).await;
    let contest_id: i64 = sqlx::query_scalar("SELECT contest_id FROM entries WHERE id = ?1")
        .bind(entry_id)
        .fetch_one(&pool)
        .await
        .unwrap();

    let cookie = app.login_as(&member.owner).await;
    let cookie = cookie.split(';').next().unwrap().to_owned();
    let request = Request::builder()
        .uri(format!(
            "/contests/{contest_id}/events?league_id={}",
            member.league.id
        ))
        .header("cookie", cookie)
        .body(Body::empty())
        .unwrap();
    let response = app.router.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.into_body();
    let (body, _) = tokio::join!(
        tokio::time::timeout(std::time::Duration::from_secs(5), body.collect()),
        async {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            app.state
                .live
                .notify(crate::contests::ContestId(contest_id));
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            app.state.live.shutdown();
        }
    );
    let body = body.expect("contest SSE stream did not close").unwrap();
    let body = String::from_utf8(body.to_bytes().to_vec()).unwrap();

    assert!(
        body.matches("data: ").count() >= 2,
        "initial and update frames: {body}"
    );
    assert!(body.contains(&format!("id=\"contest-{contest_id}-live-indicator\"")));
    assert!(body.contains(&format!(
        "hx-swap-oob=\"outerHTML:#contest-{contest_id}-live-indicator\""
    )));
    assert!(body.contains(&format!(
        "id=\"contest-{contest_id}-entries\" hx-swap-oob=\"innerHTML:#contest-{contest_id}-entries\""
    )));
    assert!(!body.contains(": shutdown"));
}

#[sqlx::test]
async fn contest_events_frames_carry_entry_minutes(pool: SqlitePool) {
    let app = TestApp::from_pool_at(pool.clone(), KICKOFF).await;
    let (_owner, member, entry_id) = setup_entry(&pool, &app, "00-A", &[scheduled_game()]).await;
    let contest_id: i64 = sqlx::query_scalar("SELECT contest_id FROM entries WHERE id = ?1")
        .bind(entry_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    let contest = crate::contests::ContestId(contest_id);
    let game = scheduled_game();
    let live = live_game(&game).expect("fixture kickoff");
    let mut alpha = factories::player("00-A", "Alpha Runner");
    alpha.espn_id = Some("live-alpha".into());
    app.nfl.seed_for_test(&[alpha], &[]).await.unwrap();
    let identity = crate::live::identity::LiveIdentityIndex::from_sources(
        &app.nfl.players().await.unwrap(),
        &[],
    );
    app.state
        .live
        .register_contest(contest, vec![game.clone()], Arc::new(identity), KICKOFF);
    let observed_at = KICKOFF + time::Duration::seconds(30);
    app.state.live.publish_event(
        contest,
        &box_score_event(
            &game,
            1,
            LiveGamePhase::InProgress,
            observed_at,
            "2",
            "08:00",
            vec![live_player_stats(
                "live-alpha",
                "Alpha Runner",
                "KC",
                "BUF",
                "RB",
                100,
            )],
        ),
        std::slice::from_ref(&live),
    );

    let cookie = app.login_as(&member.owner).await;
    let cookie = cookie.split(';').next().unwrap().to_owned();
    let request = Request::builder()
        .uri(format!(
            "/contests/{contest_id}/events?league_id={}",
            member.league.id
        ))
        .header("cookie", cookie)
        .body(Body::empty())
        .unwrap();
    let response = app.router.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let mut stream = SseReader::new(response.into_body());
    let initial = tokio::time::timeout(std::time::Duration::from_secs(5), stream.next_event())
        .await
        .expect("contest SSE initial event timed out")
        .expect("contest SSE closed before its initial event");
    assert!(
        initial.contains(&format!(
            "id=\"contest-{contest_id}-entries\" hx-swap-oob=\"innerHTML:#contest-{contest_id}-entries\""
        )),
        "entries region is the OOB swap: {initial}"
    );
    assert!(
        initial.contains("minutes-meter minutes-meter--ok"),
        "initial band is ok: {initial}"
    );
    assert!(
        initial.contains(r#"style="width: 63%;""#),
        "342 of 540 minutes fills 63%: {initial}"
    );
    assert!(
        initial.contains("342m"),
        "nine resolvable slots x 38 min each: {initial}"
    );

    app.state.live.publish_event(
        contest,
        &box_score_event(
            &game,
            2,
            LiveGamePhase::Final,
            KICKOFF + time::Duration::seconds(35),
            "4",
            "00:00",
            vec![live_player_stats(
                "live-alpha",
                "Alpha Runner",
                "KC",
                "BUF",
                "RB",
                100,
            )],
        ),
        std::slice::from_ref(&live),
    );
    let drained = tokio::time::timeout(std::time::Duration::from_secs(5), stream.next_event())
        .await
        .expect("contest SSE drained event timed out")
        .expect("contest SSE closed before its drained event");
    assert!(
        drained.contains("minutes-meter minutes-meter--low"),
        "the final game bottoms the fill bar's band: {drained}"
    );
    assert!(
        drained.contains(r#"style="width: 0%;""#),
        "no minutes left leaves an empty bar: {drained}"
    );
    assert!(
        drained.contains("0m"),
        "drained meter reads zero minutes: {drained}"
    );
    assert!(!drained.contains("342m"), "{drained}");

    app.state.live.shutdown();
    let closed = tokio::time::timeout(std::time::Duration::from_secs(5), stream.next_event())
        .await
        .expect("contest SSE stream did not close after shutdown");
    assert!(
        closed.is_none(),
        "shutdown ends the entries stream: {closed:?}"
    );
}

#[sqlx::test]
async fn live_contest_official_streams_close_after_membership_loss(pool: SqlitePool) {
    let app = TestApp::from_pool_at(pool.clone(), KICKOFF).await;
    let (_owner, member, entry_id) = setup_entry(&pool, &app, "00-A", &[scheduled_game()]).await;
    let contest_id: i64 = sqlx::query_scalar("SELECT contest_id FROM entries WHERE id = ?1")
        .bind(entry_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    let contest = crate::contests::ContestId(contest_id);

    let game = scheduled_game();
    let live_game = live_game(&game).expect("fixture kickoff");
    let identity = crate::live::identity::LiveIdentityIndex::from_sources(&[], &[]);
    app.state
        .live
        .register_contest(contest, vec![game], Arc::new(identity), KICKOFF);
    let event = live_scoreboard_event(&scheduled_game());
    app.state
        .live
        .publish_event(contest, &event, std::slice::from_ref(&live_game));

    let mut alpha = factories::player("00-A", "Alpha Runner");
    alpha.espn_id = Some("live-alpha".into());
    app.nfl.seed_for_test(&[alpha], &[]).await.unwrap();

    let cookie = app.login_as(&member.owner).await;
    let cookie = cookie.split(';').next().unwrap().to_owned();
    let entry_request = Request::builder()
        .uri(format!("/entries/{entry_id}/events"))
        .header("cookie", cookie.clone())
        .body(Body::empty())
        .unwrap();
    let entry_response = app.router.clone().oneshot(entry_request).await.unwrap();
    assert_eq!(entry_response.status(), StatusCode::OK);
    let contest_request = Request::builder()
        .uri(format!(
            "/contests/{contest_id}/events?league_id={}",
            member.league.id
        ))
        .header("cookie", cookie)
        .body(Body::empty())
        .unwrap();
    let contest_response = app.router.clone().oneshot(contest_request).await.unwrap();
    assert_eq!(contest_response.status(), StatusCode::OK);

    let mut entry_stream = SseReader::new(entry_response.into_body());
    let mut contest_stream = SseReader::new(contest_response.into_body());
    let entry_initial =
        tokio::time::timeout(std::time::Duration::from_secs(5), entry_stream.next_event())
            .await
            .expect("entry SSE initial event timed out")
            .expect("entry SSE closed before its initial event");
    let contest_initial = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        contest_stream.next_event(),
    )
    .await
    .expect("contest SSE initial event timed out")
    .expect("contest SSE closed before its initial event");
    assert!(entry_initial.contains("data: "));
    assert!(contest_initial.contains("data: "));

    let buf = NflTeamAbbr("BUF".into());
    let kc = NflTeamAbbr("KC".into());
    let rb = NflPlayerWeekStats {
        opponent: Some(buf.clone()),
        ..factories::rb_stats("00-A", 1, 100)
    };
    let buf_player = NflPlayerWeekStats {
        gsis_id: "00-BUF".into(),
        team: buf.clone(),
        opponent: Some(kc.clone()),
        ..factories::player_stats("00-BUF", 1)
    };
    app.nfl
        .seed_week_stats_for_test(
            Season(2025),
            &[rb, buf_player],
            &[
                factories::scoreless_defense_stats("KC", "BUF", 1),
                NflTeamWeekStats {
                    gsis_game_id: SLATE_GAME.into(),
                    ..factories::scoreless_defense_stats("BUF", "KC", 1)
                },
            ],
        )
        .await
        .unwrap();
    app.state.live.notify(contest);

    let entry_official =
        tokio::time::timeout(std::time::Duration::from_secs(5), entry_stream.next_event())
            .await
            .expect("entry SSE official event timed out")
            .expect("entry SSE closed before official stats");
    let contest_official = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        contest_stream.next_event(),
    )
    .await
    .expect("contest SSE official event timed out")
    .expect("contest SSE closed before official stats");
    assert!(entry_official.contains(&format!("id=\"entry-{entry_id}-live-indicator\"")));
    assert!(entry_official.contains(r#"class="size-2 rounded-full bg-warning hidden""#));
    assert!(entry_official.contains(&format!(
        "hx-swap-oob=\"outerHTML:#entry-{entry_id}-live-indicator\""
    )));
    assert!(contest_official.contains(&format!("id=\"contest-{contest_id}-live-indicator\"")));
    assert!(contest_official.contains(r#"class="size-2 rounded-full bg-warning hidden""#));
    assert!(contest_official.contains(&format!(
        "hx-swap-oob=\"outerHTML:#contest-{contest_id}-live-indicator\""
    )));

    let replacement = factories::team(&pool, TeamOptions::default()).await;
    sqlx::query("UPDATE fantasy_teams SET user_id = ?1 WHERE id = ?2")
        .bind(replacement.owner.id)
        .bind(member.team.id)
        .execute(&pool)
        .await
        .unwrap();
    app.state.live.notify(contest);

    let entry_after_access_loss =
        tokio::time::timeout(std::time::Duration::from_secs(5), entry_stream.next_event())
            .await
            .expect("entry SSE did not close after access loss");
    let contest_after_access_loss = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        contest_stream.next_event(),
    )
    .await
    .expect("contest SSE did not close after access loss");
    assert!(entry_after_access_loss.is_none());
    assert!(contest_after_access_loss.is_none());
}

#[sqlx::test]
async fn contest_events_reject_a_foreign_pinned_league(pool: SqlitePool) {
    let app = TestApp::from_pool_at(pool.clone(), KICKOFF).await;
    let (_owner, member, entry_id) = setup_entry(&pool, &app, "00-A", &[scheduled_game()]).await;
    let contest_id: i64 = sqlx::query_scalar("SELECT contest_id FROM entries WHERE id = ?1")
        .bind(entry_id)
        .fetch_one(&pool)
        .await
        .unwrap();

    app.login_as(&member.owner).await;
    let response = app
        .get(&format!(
            "/contests/{contest_id}/events?league_id={}",
            member.league.id + 1
        ))
        .await;
    response.assert_status_not_found();
}
#[sqlx::test]
async fn live_snapshot_scores_entry_before_official_stats(pool: SqlitePool) {
    let app = TestApp::from_pool_at(pool.clone(), KICKOFF).await;
    let (_owner, member, entry_id) = setup_entry(&pool, &app, "00-A", &[scheduled_game()]).await;
    let contest_id: i64 = sqlx::query_scalar("SELECT contest_id FROM entries WHERE id = ?1")
        .bind(entry_id)
        .fetch_one(&pool)
        .await
        .unwrap();

    let mut alpha = factories::player("00-A", "Alpha Runner");
    alpha.espn_id = Some("live-alpha".into());
    app.nfl.seed_for_test(&[alpha], &[]).await.unwrap();
    let game = scheduled_game();
    let live_game = live_game(&game).expect("fixture kickoff");
    let observed_at = KICKOFF + time::Duration::seconds(30);
    let event = tank01_data::PollEvent::BoxScore {
        sequence: 1,
        game: live_game.clone(),
        request: tank01_data::PollRequest {
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
                game: live_game.clone(),
                observed_at,
                phase: LiveGamePhase::InProgress,
                period: Some("2".into()),
                clock: Some("08:00".into()),
                home_score: Some(14),
                away_score: Some(7),
                players: Some(vec![LivePlayerStats {
                    espn_id: Some(EspnPlayerId("live-alpha".into())),
                    name: Some("Alpha Runner".into()),
                    team: Some(NflTeamAbbr("KC".into())),
                    position: Some("RB".into()),
                    opponent: Some(NflTeamAbbr("BUF".into())),
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
                    rushing_yards: 100,
                    receiving_yards: 0,
                }]),
                defenses: Some(vec![
                    LiveTeamStats {
                        team: NflTeamAbbr("KC".into()),
                        opponent: NflTeamAbbr("BUF".into()),
                        sacks: 1,
                        interceptions: 0,
                        fumble_recoveries: 0,
                        safeties: 0,
                        touchdowns: 0,
                        blocked_kicks: 0,
                        conversion_returns: 0,
                        points_allowed: 7,
                        dst_present: true,
                        team_stats_present: true,
                    },
                    LiveTeamStats {
                        team: NflTeamAbbr("BUF".into()),
                        opponent: NflTeamAbbr("KC".into()),
                        sacks: 0,
                        interceptions: 0,
                        fumble_recoveries: 0,
                        safeties: 0,
                        touchdowns: 0,
                        blocked_kicks: 0,
                        conversion_returns: 0,
                        points_allowed: 14,
                        dst_present: true,
                        team_stats_present: true,
                    },
                ]),
            }),
        },
    };
    let identity = crate::live::identity::LiveIdentityIndex::from_sources(
        &app.nfl.players().await.unwrap(),
        &[],
    );
    let contest = crate::contests::ContestId(contest_id);
    app.state
        .live
        .register_contest(contest, vec![game], Arc::new(identity), KICKOFF);
    app.state
        .live
        .publish_event(contest, &event, std::slice::from_ref(&live_game));

    app.login_as(&member.owner).await;
    let body = app.get(&format!("/entries/{entry_id}")).await.text();
    assert!(body.contains("100 YDS"), "live box score renders: {body}");
    assert!(body.contains(
        r#"<span class="pick-pts-text"><span class="pick-pts-whole">13</span><span class="pick-pts-dec">.00</span></span>"#
    ));
    let summary_start = body
        .find(&format!(r#"<div id="entry-{entry_id}-score-summary""#))
        .expect("score summary renders");
    let regions_start = body
        .find(&format!(r#"<div id="entry-{entry_id}-score-regions">"#))
        .expect("score regions render");
    let summary = &body[summary_start..regions_start];
    assert!(
        !summary.contains("Live") && !summary.contains("LIVE"),
        "summary has no contest-level live status: {summary}"
    );
    assert!(
        !summary.contains("badge-error"),
        "summary has no live pill: {summary}"
    );
    assert!(
        summary.contains(r#"<div class="text-2xl font-bold tnum">18.00</div>"#),
        "live lineup total renders: {summary}"
    );
    assert!(
        body.contains(r#"<div class="w-6 text-center text-xl font-bold tabular-nums">1</div>"#)
    );
}

/// Minimal parent rows an entry needs: user 1, league 1, team 1, contest 1.
/// (Companion to `setup`: direct-DB fixtures for schema-level contracts.)
async fn seed_entry_parents(pool: &SqlitePool) {
    for sql in [
        "INSERT INTO users (id, email) VALUES (1, 'a@example.com')",
        "INSERT INTO leagues (id, name) VALUES (1, 'L')",
        "INSERT INTO fantasy_teams (id, league_id, user_id, name, owner_name)
         VALUES (1, 1, 1, 'T', 'O')",
        "INSERT INTO contests (id, name) VALUES (1, 'Week 1')",
        "INSERT INTO entries (id, contest_id, fantasy_team_id) VALUES (1, 1, 1)",
    ] {
        sqlx::query(sql).execute(pool).await.unwrap();
    }
}

#[sqlx::test]
async fn entry_slots_enforce_the_def_shape_entry_slots_shape(pool: SqlitePool) {
    seed_entry_parents(&pool).await;

    // Valid player slot and valid DEF slot both insert.
    sqlx::query(
        "INSERT INTO entry_slots (entry_id, roster_slot, gsis_player_id)
         VALUES (1, 'QB', '00-0034796')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO entry_slots (entry_id, roster_slot, team_abbr)
         VALUES (1, 'DEF', 'SF')",
    )
    .execute(&pool)
    .await
    .unwrap();

    // DEF with a player id, player slot with a team, playerless player slot: all rejected.
    for sql in [
        "INSERT INTO entry_slots (entry_id, roster_slot, gsis_player_id)
         VALUES (1, 'FLEX', NULL)",
        "INSERT INTO entry_slots (entry_id, roster_slot, gsis_player_id, team_abbr)
         VALUES (1, 'TE', '00-0034796', 'SF')",
        "INSERT INTO entry_slots (entry_id, roster_slot, gsis_player_id, team_abbr)
         VALUES (1, 'RB1', NULL, 'SF')",
    ] {
        assert!(sqlx::query(sql).execute(&pool).await.is_err(), "{sql}");
    }
}

#[sqlx::test]
async fn entry_slots_reject_an_unknown_roster_slot_entry_slots_unknown_slot(pool: SqlitePool) {
    seed_entry_parents(&pool).await;

    // Only the nine named slots exist; a bare position, a tenth slot, and a
    // case variant are all rejected.
    for sql in [
        "INSERT INTO entry_slots (entry_id, roster_slot, gsis_player_id)
         VALUES (1, 'RB', '00-0034796')",
        "INSERT INTO entry_slots (entry_id, roster_slot, gsis_player_id)
         VALUES (1, 'WR4', '00-0034796')",
        "INSERT INTO entry_slots (entry_id, roster_slot, team_abbr)
         VALUES (1, 'def', 'SF')",
        "INSERT INTO entry_slots (entry_id, roster_slot, gsis_player_id)
         VALUES (1, 'qb', '00-0034796')",
    ] {
        assert!(sqlx::query(sql).execute(&pool).await.is_err(), "{sql}");
    }
}

#[sqlx::test]
async fn entry_slots_reject_the_same_player_twice_entry_slots_same_player_rejected(
    pool: SqlitePool,
) {
    seed_entry_parents(&pool).await;
    sqlx::query(
        "INSERT INTO entry_slots (entry_id, roster_slot, gsis_player_id)
         VALUES (1, 'WR1', '00-0034796')",
    )
    .execute(&pool)
    .await
    .unwrap();
    assert!(
        sqlx::query(
            "INSERT INTO entry_slots (entry_id, roster_slot, gsis_player_id)
             VALUES (1, 'FLEX', '00-0034796')",
        )
        .execute(&pool)
        .await
        .is_err()
    );
}

#[sqlx::test]
async fn deleting_an_entry_cascades_to_its_slots_entry_cascade_slots(pool: SqlitePool) {
    seed_entry_parents(&pool).await;
    sqlx::query(
        "INSERT INTO entry_slots (entry_id, roster_slot, gsis_player_id)
         VALUES (1, 'QB', '00-0034796')",
    )
    .execute(&pool)
    .await
    .unwrap();

    sqlx::query("DELETE FROM entries WHERE id = 1")
        .execute(&pool)
        .await
        .unwrap();
    let slots: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM entry_slots")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(slots, 0);
}

#[sqlx::test]
async fn deleting_a_fantasy_team_with_entries_fails_fantasy_team_cascade_rejected(
    pool: SqlitePool,
) {
    seed_entry_parents(&pool).await;

    assert!(
        sqlx::query("DELETE FROM fantasy_teams WHERE id = 1")
            .execute(&pool)
            .await
            .is_err()
    );
    let entries: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM entries")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(entries, 1);
}

#[sqlx::test]
async fn deleting_a_contest_cascades_to_its_entries_and_slots_contest_cascade_entries(
    pool: SqlitePool,
) {
    seed_entry_parents(&pool).await;
    sqlx::query(
        "INSERT INTO entry_slots (entry_id, roster_slot, gsis_player_id)
         VALUES (1, 'QB', '00-0034796')",
    )
    .execute(&pool)
    .await
    .unwrap();

    sqlx::query("DELETE FROM contests WHERE id = 1")
        .execute(&pool)
        .await
        .unwrap();

    let entries: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM entries")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(entries, 0);
    let slots: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM entry_slots")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(slots, 0);
}

#[sqlx::test]
async fn entries_are_kept_at_exactly_the_kickoff_instant_entries_kept_at_exact_kickoff(
    pool: SqlitePool,
) {
    // The entry lock is computed against `now` in the app, not the DB clock:
    // exactly at the kickoff instant the entry exists and its lineup page
    // renders (boundary is `<=`, not `<`). Built with the standard factories so
    // the owner/team/entry links satisfy the page's access rules.
    let app = TestApp::from_pool_at(pool.clone(), KICKOFF).await;
    let a = factories::team(&pool, TeamOptions::default()).await;
    let contest = factories::contest_with_games(&pool, "Week 1", &[SLATE_GAME]).await;
    let entry_id = factories::full_entry(&pool, contest, a.team.id, "00-A").await;
    app.nfl
        .seed_for_test(&[], &[scheduled_game()])
        .await
        .unwrap();
    app.login_as(&a.owner).await;
    let resp = app.get(&format!("/entries/{entry_id}")).await;
    resp.assert_status_ok();
}
