//! Authenticated contest SSE: one SlateId's score region updates, another's
//! subscription stays untouched.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use nfl_data::{
    LiveGamePhase, LiveGameSnapshot, LivePlayerStats, ProviderOutcome, ProviderResponse,
    SeasonType, TeamAbbr, TeamWeekStats,
};
use tank01_data::{PollEvent, PollRequest};
use time::macros::datetime;
use tower::ServiceExt;

use crate::live::identity::LiveIdentityIndex;
use crate::tests::TestApp;
use crate::tests::factories::{self, TeamOptions};
use crate::tests::publish_slate;

fn week(number: u8) -> nfl_data::SeasonWeek {
    nfl_data::SeasonWeek {
        season: nfl_data::Season(2026),
        week: nfl_data::Week(number),
    }
}

fn game(id: &str, week: nfl_data::SeasonWeek, kickoff: time::OffsetDateTime) -> nfl_data::Game {
    nfl_data::Game {
        game_id: id.into(),
        season_week: week,
        kickoff: Some(kickoff),
        home_team: TeamAbbr::KC,
        away_team: TeamAbbr::BUF,
        home_score: None,
        away_score: None,
    }
}

fn player(gsis: &str, name: &str, position: &str) -> nfl_data::Player {
    let mut player = factories::player(gsis, name);
    player.position = Some(position.into());
    player
}

/// Read SSE data frames until the accumulated body contains `needle`.
async fn next_fragment(body: &mut Body, accumulated: &mut String, needle: &str) {
    for _ in 0..20 {
        let frame = tokio::time::timeout(Duration::from_secs(5), body.frame())
            .await
            .expect("SSE frame arrives")
            .expect("SSE body stays open")
            .expect("SSE frame is valid");
        if let Ok(data) = frame.into_data() {
            accumulated.push_str(&String::from_utf8_lossy(&data));
            if accumulated.contains(needle) {
                return;
            }
        }
    }
    panic!("no SSE fragment containing {needle}: {accumulated}");
}

/// Authenticate against `router` and return the session cookie header value.
async fn login(router: axum::Router, user_id: i64) -> String {
    let response = router
        .oneshot(
            Request::post(format!("/__test/login/{user_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("login request");
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    response
        .headers()
        .get("set-cookie")
        .expect("login sets a session cookie")
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string()
}

#[sqlx::test]
async fn score_region_updates_for_one_slate_and_not_another(pool: sqlx::SqlitePool) {
    let nfl = Arc::new(nfl_data::NflData::in_memory().await.unwrap());
    let now = datetime!(2026-09-13 18:00 UTC);
    let app = TestApp::from_pool_with_nfl_at(pool.clone(), nfl, now).await;
    let team = factories::team(&pool, TeamOptions::default()).await;

    let week1 = week(1);
    let week2 = week(2);
    let game1 = game("2026_01_BUF_KC", week1, datetime!(2026-09-13 17:00 UTC));
    let game2 = game("2026_02_BUF_KC", week2, datetime!(2026-09-13 17:00 UTC));
    let alpha = player("00-A", "Alpha Runner", "RB");
    let slate = publish_slate(
        &app,
        week1,
        std::slice::from_ref(&game1),
        std::slice::from_ref(&alpha),
    )
    .await;
    let other = publish_slate(&app, week2, std::slice::from_ref(&game2), &[]).await;
    let entry_id = factories::full_entry(&pool, slate, team.team.id, "00-A").await;

    // A live app: the hub snapshot is the provisional score source.
    let (session_layer, sessions) = crate::sessions::layer_in_memory().await.unwrap();
    let mut state = app.state();
    let mut config = (*state.config).clone();
    config.live_dev_feed = true;
    state.config = Arc::new(config);
    state.sessions = sessions;

    let identity = Arc::new(LiveIdentityIndex::from_sources(&[alpha], &[]));
    state.live.rebase_contest(
        slate,
        vec![game1.clone()],
        identity,
        &[],
        state.now_eastern(),
        state
            .nfl
            .snapshot()
            .slate(&slate)
            .is_some_and(|slate| slate.locked(state.now_eastern())),
    );
    assert!(state.live.snapshot(slate).is_some());
    assert!(state.live.snapshot(other).is_none());

    let router = crate::router::build(state.clone(), session_layer);
    let cookie = login(router.clone(), team.owner.id).await;

    let events = |slate: nfl_data::slates::SlateId| {
        Request::get(format!(
            "/contests/{}/events?league_id={}",
            slate.0, team.league.id
        ))
        .header("cookie", cookie.clone())
        .body(Body::empty())
        .unwrap()
    };

    let first = router
        .clone()
        .oneshot(events(slate))
        .await
        .expect("first SSE response");
    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(
        first.headers().get("content-type").unwrap(),
        "text/event-stream"
    );
    let mut first_body = first.into_body();
    let mut first_text = String::new();
    next_fragment(
        &mut first_body,
        &mut first_text,
        &format!("contest-{}-entries", slate.0),
    )
    .await;
    assert!(
        first_text.contains(&format!("entry-{}-score", entry_id)),
        "initial fragment carries the score region: {first_text}"
    );
    assert!(
        !first_text.contains("23.00"),
        "no live score before the event: {first_text}"
    );

    let second = router
        .clone()
        .oneshot(events(other))
        .await
        .expect("second SSE response");
    assert_eq!(second.status(), StatusCode::OK);
    let mut second_body = second.into_body();
    let mut second_text = String::new();
    next_fragment(
        &mut second_body,
        &mut second_text,
        &format!("contest-{}-entries", other.0),
    )
    .await;

    let live = crate::live::live_game(&game1).expect("fixture game has a Tank01 game id");
    let observed = state.now_eastern();
    let stats = LivePlayerStats {
        espn_id: None,
        name: Some("Alpha Runner".into()),
        team: Some(TeamAbbr::KC),
        position: Some("RB".into()),
        opponent: Some(TeamAbbr::BUF),
        completions: 0,
        attempts: 0,
        passing_tds: 0,
        passing_interceptions: 0,
        rushing_attempts: 10,
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
    };
    let defense = TeamWeekStats {
        season: nfl_data::Season(2026),
        week: nfl_data::Week(1),
        season_type: SeasonType::Reg,
        team: TeamAbbr::KC,
        opponent: TeamAbbr::BUF,
        game_id: game1.game_id.clone(),
        sacks: 0,
        interceptions: 0,
        fumble_recoveries: 0,
        safeties: 0,
        touchdowns: 0,
        blocked_kicks: 0,
        conversion_returns: 0,
        points_allowed: 0,
    };
    let snapshot = LiveGameSnapshot {
        game: live.clone(),
        observed_at: observed,
        phase: LiveGamePhase::InProgress,
        period: Some("1".into()),
        clock: Some("10:00".into()),
        home_score: Some(7),
        away_score: Some(0),
        players: Some(vec![stats]),
        defenses: Some(vec![nfl_data::LiveTeamStats {
            team: TeamAbbr::KC,
            opponent: TeamAbbr::BUF,
            sacks: defense.sacks,
            interceptions: defense.interceptions,
            fumble_recoveries: defense.fumble_recoveries,
            safeties: defense.safeties,
            touchdowns: defense.touchdowns,
            blocked_kicks: defense.blocked_kicks,
            conversion_returns: defense.conversion_returns,
            points_allowed: defense.points_allowed,
            dst_present: true,
            team_stats_present: true,
        }]),
    };
    let event = PollEvent::BoxScore {
        sequence: 1,
        game: live.clone(),
        request: PollRequest {
            endpoint: "boxscore".into(),
            query: BTreeMap::new(),
        },
        play_by_play: false,
        changed: true,
        response: ProviderResponse {
            requested_at: observed,
            received_at: observed,
            elapsed_ms: 0,
            http_status: Some(200),
            headers: BTreeMap::new(),
            raw_body: None,
            outcome: ProviderOutcome::Value(snapshot),
        },
    };
    state
        .live
        .publish_event(slate, &event, std::slice::from_ref(&live));

    let mut changed_text = first_text;
    next_fragment(&mut first_body, &mut changed_text, "23.00").await;
    assert!(
        changed_text.contains(&format!("contest-{}-entry-{entry_id}-score", slate.0)),
        "the changed score region belongs to the subscribed slate: {changed_text}"
    );

    let untouched = tokio::time::timeout(Duration::from_millis(400), second_body.frame()).await;
    assert!(
        untouched.is_err(),
        "the other SlateId's SSE subscription receives no frame"
    );
}
