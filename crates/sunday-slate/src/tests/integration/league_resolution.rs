//! The `leagues::resolve` ladder: entity-sticky switching, ambient session
//! resolution, stale-session fallback.

use crate::tests::factories::{self, LeagueOptions, TeamOptions, UserOptions};
use crate::tests::test_app::TestApp;
use http::StatusCode;
use nfl_data::Game;
use sqlx::SqlitePool;
use time::OffsetDateTime;
use time::macros::datetime;

/// A standings row for `name`.
fn standings_row(name: &str) -> String {
    format!(r#"<div class="font-medium">{name}</div>"#)
}

struct TwoLeagues {
    app: TestApp,
    team_a_name: String,
    team_b_name: String,
    team_b_id: i64,
    league_a_id: i64,
    league_b_id: i64,
    league_b_name: String,
}

/// One user with a team in each of two leagues (created lowest-id first).
async fn two_league_member() -> TwoLeagues {
    let app = TestApp::new().await;
    let league_a = factories::league(&app.pool, LeagueOptions::default()).await;
    let league_b = factories::league(&app.pool, LeagueOptions::default()).await;
    let user = factories::user(&app.pool, UserOptions::default()).await;
    let team_a = factories::team(
        &app.pool,
        TeamOptions {
            league: Some(league_a.league.clone()),
            owner: Some(user.user.clone()),
            name: Some("Alpha Squad".into()),
            ..Default::default()
        },
    )
    .await;
    let team_b = factories::team(
        &app.pool,
        TeamOptions {
            league: Some(league_b.league.clone()),
            owner: Some(user.user.clone()),
            name: Some("Bravo Squad".into()),
            ..Default::default()
        },
    )
    .await;
    app.login(&user).await;
    TwoLeagues {
        app,
        team_a_name: team_a.team.name,
        team_b_name: team_b.team.name,
        team_b_id: team_b.team.id,
        league_a_id: league_a.league.id,
        league_b_id: league_b.league.id,
        league_b_name: league_b.league.name,
    }
}

/// With no session value, `/` resolves to the member's lowest-id league.
#[tokio::test]
async fn fallback_is_the_lowest_id_team_league() {
    let t = two_league_member().await;
    // The active league shows in the standings rows.
    let home = t.app.get("/standings").await;
    home.assert_status_ok();
    let html = home.text();
    assert!(html.contains(&standings_row(&t.team_a_name)));
    assert!(!html.contains(&standings_row(&t.team_b_name)));
}

/// Viewing a team page (or its editors) in the member's other league switches
/// the active league: the next `/` renders that league's standings.
#[tokio::test]
async fn entity_page_sticky_switches_the_active_league() {
    for suffix in ["", "/edit", "/logo"] {
        let t = two_league_member().await;
        let page = t.app.get(&format!("/teams/{}{suffix}", t.team_b_id)).await;
        page.assert_status_ok();
        // Fresh session: this is a first-time default, not a switch away
        // from an already-active league, so no "Now viewing …" toast.
        assert!(!page.text().contains("Now viewing"), "{suffix}");

        let home = t.app.get("/standings").await;
        home.assert_status_ok();
        let html = home.text();
        assert!(html.contains(&standings_row(&t.team_b_name)), "{suffix}");
        assert!(!html.contains(&standings_row(&t.team_a_name)), "{suffix}");
        assert!(html.contains(&t.league_b_name), "{suffix}");
    }
}

/// Once a league is already active, following a link into the member's
/// other league is a real switch: the resolve ladder's rung-1 marker
/// (`stored.is_some() && stored != Some(league_id)`) fires and the shell
/// renders a "Now viewing …" toast on the very page that switched.
#[tokio::test]
async fn entity_page_switch_from_an_active_league_shows_the_toast() {
    let t = two_league_member().await;

    // Establish league A as the active league first, so the move to league
    // B below is a switch rather than a first-time default.
    let switched = t
        .app
        .post_htmx("/leagues/switch", &format!("league_id={}", t.league_a_id))
        .await;
    switched.assert_status_ok();

    let page = t.app.get(&format!("/teams/{}", t.team_b_id)).await;
    page.assert_status_ok();
    let html = page.text();
    assert!(html.contains(&format!("Now viewing {}", t.league_b_name)));

    // And the active league really did move to B.
    let home = t.app.get("/standings").await;
    home.assert_status_ok();
    let html = home.text();
    assert!(html.contains(&standings_row(&t.team_b_name)));
    assert!(!html.contains(&standings_row(&t.team_a_name)));
}

/// A non-member requesting another league's team page gets 404, and their
/// active league does not move.
#[tokio::test]
async fn non_member_entity_page_is_404_and_not_sticky() {
    let app = TestApp::new().await;
    let league_a = factories::league(&app.pool, LeagueOptions::default()).await;
    let league_b = factories::league(&app.pool, LeagueOptions::default()).await;
    let user = factories::user(&app.pool, UserOptions::default()).await;
    let team_a = factories::team(
        &app.pool,
        TeamOptions {
            league: Some(league_a.league.clone()),
            owner: Some(user.user.clone()),
            name: Some("Alpha Squad".into()),
            ..Default::default()
        },
    )
    .await;
    let other = factories::team(
        &app.pool,
        TeamOptions {
            league: Some(league_b.league.clone()),
            name: Some("Stranger Danger".into()),
            ..Default::default()
        },
    )
    .await;
    app.login(&user).await;

    for suffix in ["", "/edit", "/logo"] {
        let page = app.get(&format!("/teams/{}{suffix}", other.team.id)).await;
        page.assert_status(StatusCode::NOT_FOUND);
    }

    let home = app.get("/standings").await;
    home.assert_status_ok();
    assert!(home.text().contains(&standings_row(&team_a.team.name)));
}

/// A session pointing at a league the member lost falls back cleanly.
#[tokio::test]
async fn stale_active_league_is_cleared_and_falls_back() {
    let t = two_league_member().await;
    // Stick to league B, then delete B's teams and B itself.
    t.app
        .get(&format!("/teams/{}", t.team_b_id))
        .await
        .assert_status_ok();
    sqlx::query("DELETE FROM fantasy_teams WHERE league_id = ?1")
        .bind(t.league_b_id)
        .execute(&t.app.pool)
        .await
        .expect("delete teams");
    sqlx::query("DELETE FROM leagues WHERE id = ?1")
        .bind(t.league_b_id)
        .execute(&t.app.pool)
        .await
        .expect("delete league");

    let home = t.app.get("/standings").await;
    home.assert_status_ok();
    assert!(home.text().contains(&standings_row(&t.team_a_name)));
}

/// An empty league someone else is bootstrapping never bounces a member of a
/// healthy league.
#[tokio::test]
async fn member_of_healthy_league_ignores_someone_elses_empty_league() {
    let app = TestApp::new().await;
    let league_a = factories::league(&app.pool, LeagueOptions::default()).await;
    factories::bare_league(&app.pool, Some("Empty League".into())).await;
    let user = factories::user(&app.pool, UserOptions::default()).await;
    let team = factories::team(
        &app.pool,
        TeamOptions {
            league: Some(league_a.league.clone()),
            owner: Some(user.user.clone()),
            ..Default::default()
        },
    )
    .await;
    app.login(&user).await;

    let home = app.get("/standings").await;
    home.assert_status_ok();
    assert!(home.text().contains(&team.team.name));
}

/// An admin who abandons league creation resolves back to their own league;
/// the new league stays reachable at /teams/new via the pending value.
#[tokio::test]
async fn abandoned_league_creation_falls_back_and_stays_completable() {
    let app = TestApp::new().await;
    let pool = app.pool.clone();
    let admin = factories::user(
        &pool,
        UserOptions {
            is_admin: true,
            ..Default::default()
        },
    )
    .await;
    let home = factories::team(
        &pool,
        TeamOptions {
            owner: Some(admin.user.clone()),
            ..Default::default()
        },
    )
    .await;
    app.login(&admin).await;

    // Create a league but abandon the bounce to /teams/new.
    app.post("/leagues", "name=Fresh+League").await;

    // The admin's own league resolves, not the team-less one.
    let resp = app.get("/standings").await;
    resp.assert_status_ok();
    assert!(resp.text().contains(&home.league.name));

    // The creation flow is still completable: /teams/new targets the
    // pending league, and the created team lands there.
    let resp = app.get("/teams/new").await;
    resp.assert_status_ok();
    app.post("/teams", "name=Fresh+Team&owner_name=Admin").await;
    let resp = app.get("/standings").await;
    resp.assert_status_ok();
    assert!(resp.text().contains("Fresh League"));
}

/// The one game on the entry contest's slate.
const SLATE_GAME: &str = "2025_01_BUF_KC";

/// Kickoff: Sunday 2025-09-07 13:00 ET.
const KICKOFF: OffsetDateTime = datetime!(2025-09-07 17:00 UTC);

/// The entry page is an entity route too: opening an entry in the league the
/// user is not currently in switches the active league.
#[sqlx::test]
async fn entry_page_sticky_switches_the_active_league(pool: SqlitePool) {
    let app = TestApp::from_pool_at(pool.clone(), KICKOFF).await;

    // Created first, so it holds the lower id and is where the fallback starts.
    let fallback_league = factories::league(&pool, LeagueOptions::default()).await;

    // The entry's league comes second; its team owns the entry.
    let owner = factories::team(
        &pool,
        TeamOptions {
            name: Some("Gridiron Giants".into()),
            ..Default::default()
        },
    )
    .await;
    let contest = factories::contest_with_games(&pool, "Week 1", &[SLATE_GAME]).await;
    let entry_id = factories::full_entry(&pool, contest, owner.team.id, "00-A").await;
    let fillers: Vec<_> = (1..=7)
        .map(|i| factories::player(&format!("00-F{i}"), &format!("Filler Player{i}")))
        .collect();
    let scheduled = Game {
        kickoff: Some(KICKOFF),
        ..factories::game(SLATE_GAME, 1)
    };
    app.nfl
        .seed_for_test(&fillers, std::slice::from_ref(&scheduled))
        .await
        .unwrap();
    app.nfl
        .seed_for_test(&[factories::player("00-A", "Alpha Runner")], &[])
        .await
        .unwrap();

    // The same person also plays in the lower-id league.
    factories::team(
        &pool,
        TeamOptions {
            league: Some(fallback_league.league.clone()),
            owner: Some(owner.owner.clone()),
            name: Some("Fallback Squad".into()),
            ..Default::default()
        },
    )
    .await;

    app.login_as(&owner.owner).await;

    // Without a session value the fallback lands in the lower-id league.
    // (`/` redirects to the live contest here, which is league-agnostic, so
    // the standings page is where the active league shows.)
    let home = app.get("/standings").await;
    home.assert_status_ok();
    assert!(home.text().contains(&standings_row("Fallback Squad")));
    assert!(!home.text().contains(&standings_row("Gridiron Giants")));

    // GET /entries/{id} renders (200), and the next page shows the entry's
    // league — the sticky switch walked entry → team → league.
    let page = app.get(&format!("/entries/{entry_id}")).await;
    page.assert_status_ok();
    let home = app.get("/standings").await;
    home.assert_status_ok();
    assert!(home.text().contains(&standings_row("Gridiron Giants")));
}

/// An admin with no team in a league can no longer use it. A foreign
/// league's team page is 404 and does not sticky-switch.
#[tokio::test]
async fn admin_cannot_use_a_league_without_a_team() {
    let app = TestApp::new().await;
    let pool = app.pool.clone();
    let admin = factories::user(
        &pool,
        UserOptions {
            is_admin: true,
            ..Default::default()
        },
    )
    .await;
    // League A: the admin's own. League B: someone else's, with a team.
    factories::team(
        &pool,
        TeamOptions {
            owner: Some(admin.user.clone()),
            ..Default::default()
        },
    )
    .await;
    let foreign = factories::team(&pool, TeamOptions::default()).await;
    app.login(&admin).await;

    let resp = app.get(&format!("/teams/{}", foreign.team.id)).await;
    resp.assert_status(StatusCode::NOT_FOUND);
}

/// Journey: a two-league member sees the switcher in the drawer and switches
/// leagues with it.
#[tokio::test]
async fn drawer_switcher_switches_leagues() {
    let t = two_league_member().await;

    // The drawer offers both leagues.
    let home = t.app.get("/standings").await;
    home.assert_status_ok();
    assert!(home.text().contains(r#"hx-post="/leagues/switch""#));

    let switched = t
        .app
        .post_htmx("/leagues/switch", &format!("league_id={}", t.league_b_id))
        .await;
    switched.assert_status_ok();
    assert_eq!(switched.header("hx-redirect"), "/");

    let home = t.app.get("/standings").await;
    home.assert_status_ok();
    assert!(home.text().contains(&t.team_b_name));
}

/// Nobody can switch into a league they have no team in — not even the
/// admin.
#[tokio::test]
async fn switch_is_membership_gated_for_everyone() {
    let app = TestApp::new().await;
    let league_a = factories::league(&app.pool, LeagueOptions::default()).await;
    let league_b = factories::league(&app.pool, LeagueOptions::default()).await;
    let member = factories::user(&app.pool, UserOptions::default()).await;
    factories::team(
        &app.pool,
        TeamOptions {
            league: Some(league_a.league.clone()),
            owner: Some(member.user.clone()),
            ..Default::default()
        },
    )
    .await;
    let admin = factories::user(
        &app.pool,
        UserOptions {
            is_admin: true,
            ..Default::default()
        },
    )
    .await;
    factories::team(
        &app.pool,
        TeamOptions {
            league: Some(league_a.league.clone()),
            owner: Some(admin.user.clone()),
            ..Default::default()
        },
    )
    .await;

    app.login(&member).await;
    let denied = app
        .post(
            "/leagues/switch",
            &format!("league_id={}", league_b.league.id),
        )
        .await;
    denied.assert_status(StatusCode::FORBIDDEN);

    let mut app = app;
    app.clear_cookies();
    app.login(&admin).await;
    let also_denied = app
        .post(
            "/leagues/switch",
            &format!("league_id={}", league_b.league.id),
        )
        .await;
    also_denied.assert_status(StatusCode::FORBIDDEN);
}
