//! Entry details page: one entry's scored lineup, with access rules.

use crate::entries::Lineup;
use crate::live;
use crate::standings::service as standings_service;
use http::StatusCode;
use nfl_data::{
    Game, PlayerWeekStats as NflPlayerWeekStats, Season, TeamAbbr,
    TeamWeekStats as NflTeamWeekStats,
};
use nfl_data::{Player, SeasonWeek, Week, slates::SlateId};
use sqlx::SqlitePool;

use std::{slice, sync::Arc};
use time::OffsetDateTime;
use time::macros::datetime;
use wiremock::MockServer;

use crate::tests::factories::{self, GeneratedTeam, TeamOptions};
use crate::tests::nfl::mocked_nfl;
use crate::tests::test_app::TestApp;
use crate::tests::utils::as_rendered;
use crate::tests::{publish_slate, upload_slate};

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

/// The 2025 week-1 regular-season week the fixture publishes.
fn week1() -> SeasonWeek {
    SeasonWeek {
        season: Season(2025),
        week: Week(1),
    }
}

/// A 2025-season test app around mocked nfl-data. The returned mock keeps the
/// nflverse mocks alive for the test's lifetime.
async fn app_at(pool: &SqlitePool, now: OffsetDateTime) -> (TestApp, MockServer) {
    let (nfl, server) = mocked_nfl(Season(2025)).await;
    let app = TestApp::from_pool_with_nfl_at(pool.clone(), nfl, now).await;
    (app, server)
}

/// Every lineup pick as a priced KC player: `rb1` at RB1 plus the seven
/// fillers the full entry writes.
fn lineup_players(rb1: &str) -> Vec<Player> {
    let named = |gsis: &str, name: &str, position: &str| {
        let mut player = factories::player(gsis, name);
        player.position = Some(position.into());
        player
    };
    let rb1_name = if rb1 == "00-A" {
        "Alpha Runner"
    } else {
        "Beta Runner"
    };
    vec![
        named(rb1, rb1_name, "RB"),
        named("00-F1", "Filler Player1", "QB"),
        named("00-F2", "Filler Player2", "RB"),
        named("00-F3", "Filler Player3", "WR"),
        named("00-F4", "Filler Player4", "WR"),
        named("00-F5", "Filler Player5", "WR"),
        named("00-F6", "Filler Player6", "TE"),
        named("00-F7", "Filler Player7", "RB"),
    ]
}

/// Two fantasy teams in one league, a published "Week 1" slate whose only
/// game is `SLATE_GAME`, and an entry for team `a` with `rb1` at RB1.
/// Returns (owner team, other-member team, entry id, slate id).
async fn setup_entry(
    pool: &SqlitePool,
    app: &TestApp,
    rb1: &str,
    scheduled: &[Game],
) -> (GeneratedTeam, GeneratedTeam, i64, SlateId) {
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

    let slate = publish_slate(app, week1(), scheduled, &lineup_players(rb1)).await;
    let entry_id = factories::full_entry(pool, slate, a.team.id, rb1).await;

    (a, b, entry_id, slate)
}

/// `setup_entry` with the schedule loaded and "Alpha Runner" at RB1: 100
/// rushing yards (10.00) plus the 100+ RuY Gm bonus (3.00) = 13.00. QB
/// "Filler Player1" throws for 200 yards = 8.00. The page total of 21.00
/// matches no single row, so a total assertion cannot pass on a row instead.
async fn setup(pool: &SqlitePool, app: &TestApp) -> (GeneratedTeam, GeneratedTeam, i64) {
    let (a, b, entry_id, _slate) = setup_entry(pool, app, "00-A", &[scheduled_game()]).await;
    let buf = TeamAbbr::BUF;
    let kc = TeamAbbr::KC;
    let rb = NflPlayerWeekStats {
        opponent: Some(buf),
        ..factories::rb_stats("00-A", 1, 100)
    };
    let qb = NflPlayerWeekStats {
        passing_yards: 200,
        opponent: Some(buf),
        ..factories::player_stats("00-F1", 1)
    };
    let buf_player = NflPlayerWeekStats {
        gsis_id: "00-BUF".into(),
        team: buf,
        opponent: Some(kc),
        ..factories::player_stats("00-BUF", 1)
    };
    let buf_defense = NflTeamWeekStats {
        game_id: SLATE_GAME.into(),
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
    (a, b, entry_id)
}

/// The entry's contest is the only contest, live at the pinned clock — the
/// Current Contest — so the Current tab lights.
/// A second, sooner-kicking-off contest steals Current from the entry's
/// contest, so the entry page lights Contests instead.
#[sqlx::test]
async fn unknown_entry_is_404(pool: SqlitePool) {
    let (app, _mock) = app_at(&pool, KICKOFF).await;
    let (a, _b, _entry_id) = setup(&pool, &app).await;

    app.login_as(&a.owner).await;
    let resp = app.get("/entries/999999").await;
    resp.assert_status_not_found();
}

#[sqlx::test]
async fn user_outside_the_league_gets_404(pool: SqlitePool) {
    let (app, _mock) = app_at(&pool, KICKOFF).await;
    let (_a, _b, entry_id) = setup(&pool, &app).await;
    // A fantasy team in a different league (the factory default creates one).
    let outsider = factories::team(&pool, TeamOptions::default()).await;

    app.login_as(&outsider.owner).await;
    let resp = app.get(&format!("/entries/{entry_id}")).await;
    resp.assert_status_not_found();
}

#[sqlx::test]
async fn unauthenticated_request_is_redirected_to_login(pool: SqlitePool) {
    let (app, _mock) = app_at(&pool, KICKOFF).await;
    let (_a, _b, entry_id) = setup(&pool, &app).await;

    // No login_as call: this request carries no session cookie.
    let resp = app.get(&format!("/entries/{entry_id}")).await;
    assert_eq!(resp.status_code(), StatusCode::TEMPORARY_REDIRECT);
}

#[sqlx::test]
async fn non_owner_before_kickoff_sees_note_not_picks(pool: SqlitePool) {
    let (app, _mock) = app_at(&pool, BEFORE_KICKOFF).await;
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
async fn rank_badge_hidden_before_results(pool: SqlitePool) {
    // Rank and picks come from one branch, so the only viewer without a rank
    // is the one without picks: a non-owner before kickoff.
    let (app, _mock) = app_at(&pool, BEFORE_KICKOFF).await;
    let (_a, b, entry_id) = setup(&pool, &app).await;

    app.login_as(&b.owner).await;
    let body = app.get(&format!("/entries/{entry_id}")).await.text();
    assert!(
        !body.contains(r#"class="w-6 text-center text-xl font-bold tabular-nums""#),
        "no rank badge before results"
    );
}

#[sqlx::test]
async fn contest_entry_link_reaches_lineup_page(pool: SqlitePool) {
    let (app, _mock) = app_at(&pool, KICKOFF).await;
    let (_owner, member, entry_id) = setup(&pool, &app).await;
    let contest_id: i64 = sqlx::query_scalar("SELECT slate_id FROM entries WHERE id = ?1")
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
    let (app, _mock) = app_at(&pool, KICKOFF).await;
    let (_owner, _member, entry_id) = setup(&pool, &app).await;
    let outsider = factories::team(&pool, TeamOptions::default()).await;

    app.login_as(&outsider.owner).await;
    let response = app.get(&format!("/entries/{entry_id}/events")).await;
    response.assert_status_not_found();
}

#[sqlx::test]
async fn contest_events_reject_a_foreign_pinned_league(pool: SqlitePool) {
    let (app, _mock) = app_at(&pool, KICKOFF).await;
    let (_owner, member, entry_id, _slate) =
        setup_entry(&pool, &app, "00-A", &[scheduled_game()]).await;
    let contest_id: i64 = sqlx::query_scalar("SELECT slate_id FROM entries WHERE id = ?1")
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
/// Minimal parent rows an entry needs: user 1, league 1, team 1, and a
/// 2026 week-1 slate id. (Companion to `setup`: direct-DB fixtures for
/// schema-level contracts.)
async fn seed_entry_parents(pool: &SqlitePool) {
    for sql in [
        "INSERT INTO users (id, email) VALUES (1, 'a@example.com')",
        "INSERT INTO leagues (id, name) VALUES (1, 'L')",
        "INSERT INTO fantasy_teams (id, league_id, user_id, name, owner_name)
         VALUES (1, 1, 1, 'T', 'O')",
        "INSERT INTO entries (id, slate_id, fantasy_team_id) VALUES (1, 20260011, 1)",
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
async fn entries_are_kept_at_exactly_the_kickoff_instant_entries_kept_at_exact_kickoff(
    pool: SqlitePool,
) {
    // The entry lock is computed against `now` in the app, not the DB clock:
    // exactly at the kickoff instant the entry exists and its lineup page
    // renders (boundary is `<=`, not `<`). Built with the standard factories so
    // the owner/team/entry links satisfy the page's access rules.
    let (app, _mock) = app_at(&pool, KICKOFF).await;
    let a = factories::team(&pool, TeamOptions::default()).await;
    let slate = publish_slate(&app, week1(), &[scheduled_game()], &lineup_players("00-A")).await;
    let entry_id = factories::full_entry(&pool, slate, a.team.id, "00-A").await;
    app.login_as(&a.owner).await;
    let resp = app.get(&format!("/entries/{entry_id}")).await;
    resp.assert_status_ok();
}

/// A migrated 2026 entry keeps its id and resolves its lineup and contest
/// back-link from the mapped SlateId, with the scored total unchanged.
#[sqlx::test]
async fn migrated_2026_entry_keeps_its_id_and_links_its_slate(pool: SqlitePool) {
    const KICKOFF_2026: OffsetDateTime = datetime!(2026-09-13 17:00 UTC);
    let (nfl, _mock) = mocked_nfl(Season(2026)).await;
    let app = TestApp::from_pool_with_nfl_at(pool.clone(), nfl, KICKOFF_2026).await;
    let a = factories::team(
        &pool,
        TeamOptions {
            name: Some("Gridiron Giants".into()),
            ..Default::default()
        },
    )
    .await;
    let b = factories::team(
        &pool,
        TeamOptions {
            league: Some(a.league.clone()),
            name: Some("Turf Titans".into()),
            ..Default::default()
        },
    )
    .await;

    let week = SeasonWeek {
        season: Season(2026),
        week: Week(1),
    };
    let game = Game {
        game_id: "2026_01_BUF_KC".into(),
        season_week: week,
        kickoff: Some(KICKOFF_2026),
        ..factories::game(SLATE_GAME, 1)
    };
    let slate = publish_slate(&app, week, slice::from_ref(&game), &lineup_players("00-A")).await;
    assert_eq!(slate.0, 20260011);
    let entry_id = factories::full_entry(&pool, slate, a.team.id, "00-A").await;

    let buf = TeamAbbr::BUF;
    let kc = TeamAbbr::KC;
    let rb = NflPlayerWeekStats {
        season_week: week,
        opponent: Some(buf),
        ..factories::rb_stats("00-A", 1, 100)
    };
    let qb = NflPlayerWeekStats {
        season_week: week,
        passing_yards: 200,
        opponent: Some(buf),
        ..factories::player_stats("00-F1", 1)
    };
    let buf_player = NflPlayerWeekStats {
        season_week: week,
        gsis_id: "00-BUF".into(),
        team: buf,
        opponent: Some(kc),
        ..factories::player_stats("00-BUF", 1)
    };
    let buf_defense = NflTeamWeekStats {
        season_week: week,
        game_id: "2026_01_BUF_KC".into(),
        ..factories::scoreless_defense_stats("BUF", "KC", 1)
    };
    let kc_defense = NflTeamWeekStats {
        season_week: week,
        game_id: "2026_01_BUF_KC".into(),
        ..factories::scoreless_defense_stats("KC", "BUF", 1)
    };
    app.nfl
        .seed_week_stats_for_test(
            Season(2026),
            &[rb, qb, buf_player],
            &[kc_defense, buf_defense],
        )
        .await
        .unwrap();
    upload_slate(&app, week, slice::from_ref(&game), &lineup_players("00-A")).await;

    app.login_as(&b.owner).await;
    let body = app.get(&format!("/entries/{entry_id}")).await.text();
    assert!(body.contains("Lineup Scores"), "{body}");
    assert!(body.contains("Alpha Runner"), "the lineup resolves: {body}");
    assert!(
        body.contains(r#"href="/contests/20260011""#),
        "the contest back-link carries the mapped SlateId: {body}"
    );
    assert!(
        body.contains("21.00"),
        "the scored total is unchanged: {body}"
    );
}

fn stored_lineup_players() -> Vec<Player> {
    let mut players = lineup_players("00-A");
    let mut beta = factories::player("00-B", "Beta Runner");
    beta.position = Some("RB".into());
    players.push(beta);
    players
}

fn stored_lineup(rb1: &str) -> Lineup {
    use crate::entries::NflPlayerId;
    Lineup {
        qb: NflPlayerId("00-F1".into()),
        rb1: NflPlayerId(rb1.into()),
        rb2: NflPlayerId("00-F2".into()),
        wr1: NflPlayerId("00-F3".into()),
        wr2: NflPlayerId("00-F4".into()),
        wr3: NflPlayerId("00-F5".into()),
        te: NflPlayerId("00-F6".into()),
        flex: NflPlayerId("00-F7".into()),
        def: TeamAbbr::KC,
    }
}

async fn seed_stored_results(
    app: &TestApp,
    alpha_yards: i32,
    alpha_tds: u32,
    qb_yards: i32,
    qb_interceptions: u32,
    buf_side: bool,
) {
    let buf = TeamAbbr::BUF;
    let alpha = NflPlayerWeekStats {
        opponent: Some(buf),
        rushing_tds: alpha_tds,
        ..factories::rb_stats("00-A", 1, alpha_yards)
    };
    let beta = NflPlayerWeekStats {
        opponent: Some(buf),
        ..factories::rb_stats("00-B", 1, 100)
    };
    let qb = NflPlayerWeekStats {
        passing_yards: qb_yards,
        passing_interceptions: qb_interceptions,
        opponent: Some(buf),
        ..factories::player_stats("00-F1", 1)
    };
    let kc = NflTeamWeekStats {
        game_id: SLATE_GAME.into(),
        sacks: 3,
        interceptions: 1,
        points_allowed: 10,
        ..factories::defense_stats("KC", "BUF", 1)
    };
    let buf_defense = NflTeamWeekStats {
        game_id: SLATE_GAME.into(),
        ..factories::scoreless_defense_stats("BUF", "KC", 1)
    };
    let teams = if buf_side {
        vec![kc, buf_defense]
    } else {
        vec![kc]
    };
    app.nfl
        .seed_week_stats_for_test(Season(2025), &[alpha, beta, qb], &teams)
        .await
        .unwrap();
}

fn text_after<'a>(html: &'a str, marker: &str, end: &str) -> &'a str {
    let start = html
        .find(marker)
        .unwrap_or_else(|| panic!("missing {marker:?}"))
        + marker.len();
    let rest = &html[start..];
    let stop = rest
        .find(end)
        .unwrap_or_else(|| panic!("unterminated {marker:?}"));
    &rest[..stop]
}

fn entry_total(html: &str, entry: i64) -> String {
    let summary = html
        .find(&format!(r#"id="entry-{entry}-score-summary""#))
        .expect("score summary");
    let value = text_after(
        &html[summary..],
        r#"class="text-2xl font-bold tnum">"#,
        "</div>",
    );
    value.trim().to_string()
}

fn row_points(html: &str, entry: i64, dom_id: &str) -> String {
    let region = text_after(
        html,
        &format!(r#"id="entry-{entry}-{dom_id}-points""#),
        "</div>",
    );
    let whole = text_after(region, r#"class="pick-pts-whole">"#, "</span>");
    let dec = text_after(region, r#"class="pick-pts-dec">"#, "</span>");
    format!("{whole}{dec}")
}

fn breakdown_points(html: &str, entry: i64, dom_id: &str) -> Vec<String> {
    let region = text_after(
        html,
        &format!(r#"id="entry-{entry}-{dom_id}-breakdown""#),
        "
</div>",
    );
    region
        .split(r#"class="font-semibold text-base-content">"#)
        .skip(1)
        .map(|rest| rest[..rest.find("</span>").expect("line value")].to_string())
        .collect()
}

fn contest_entry_score(html: &str, slate: SlateId, entry: i64) -> String {
    let region = text_after(
        html,
        &format!(r#"id="contest-{}-entry-{entry}-score""#, slate.0),
        "</a>",
    );
    let value = text_after(region, r#"class="text-lg font-semibold tnum">"#, "</div>");
    value
        .rsplit('>')
        .next()
        .unwrap_or_default()
        .trim()
        .to_string()
}

fn contest_list_row(html: &str, slate: SlateId) -> &str {
    let marker = format!(r#"href="/contests/{}""#, slate.0);
    let start = html.find(&marker).expect("contest list row");
    let rest = &html[start..];
    &rest[..rest.find("</a>").unwrap_or(rest.len())]
}

fn team_row_points(html: &str, slate: SlateId) -> String {
    let row = contest_list_row(html, slate);
    text_after(
        row,
        r#"class="self-center text-lg font-semibold tnum">"#,
        "</div>",
    )
    .trim()
    .to_string()
}

struct Surface<'a> {
    team: i64,
    name: &'a str,
    entry: i64,
    points: &'a str,
    rb1: &'a str,
}

async fn assert_stored_surfaces(
    app: &TestApp,
    slate: SlateId,
    league_id: i64,
    alpha: Surface<'_>,
    beta: Surface<'_>,
) {
    let alpha_points: f64 = alpha.points.parse().unwrap();
    let beta_points: f64 = beta.points.parse().unwrap();
    // The existing rank gate hides all scores when nobody beats zero.
    let ranked = alpha_points.max(beta_points) > 0.0;
    let details = app.get(&format!("/contests/{}", slate.0)).await.text();
    for side in [&alpha, &beta] {
        let shown = if ranked { side.points } else { "" };
        assert_eq!(
            contest_entry_score(&details, slate, side.entry),
            shown,
            "contest details"
        );
    }

    for side in [&alpha, &beta] {
        let page = app.get(&format!("/teams/{}", side.team)).await.text();
        assert_eq!(team_row_points(&page, slate), side.points, "My Team");

        let body = app.get(&format!("/entries/{}", side.entry)).await.text();
        assert_eq!(entry_total(&body, side.entry), side.points, "entry total");
        assert_eq!(
            row_points(&body, side.entry, "rb1"),
            side.rb1,
            "entry breakdown"
        );
    }

    let standings = standings_service::standings_for_league(&app.state(), league_id)
        .await
        .unwrap();
    let total = |team: i64| {
        standings
            .iter()
            .find(|row| row.fantasy_team.0 == team)
            .and_then(|row| row.total)
    };
    assert_eq!(
        total(alpha.team),
        Some(alpha.points.parse::<f64>().unwrap()),
        "standings"
    );
    assert_eq!(
        total(beta.team),
        Some(beta.points.parse::<f64>().unwrap()),
        "standings"
    );

    let list = app.get("/contests").await.text();
    let row = contest_list_row(&list, slate);
    if ranked {
        let (winner, loser) = if alpha_points > beta_points {
            (&alpha, &beta)
        } else {
            (&beta, &alpha)
        };
        assert!(row.contains(winner.name), "the winner is listed: {row}");
        assert!(row.contains(winner.points), "with its total: {row}");
        assert!(!row.contains(loser.name), "and only the winner: {row}");
    } else {
        assert!(
            !row.contains("ph-trophy"),
            "no winner without a positive score: {row}"
        );
        for points in [alpha.points, beta.points] {
            assert!(!row.contains(points), "no total is listed: {row}");
        }
    }
}

async fn seed_negative_week(app: &TestApp) {
    let losers = [
        "00-A", "00-F1", "00-F2", "00-F3", "00-F4", "00-F5", "00-F6", "00-F7",
    ];
    let mut players: Vec<NflPlayerWeekStats> = losers
        .iter()
        .map(|gsis| NflPlayerWeekStats {
            opponent: Some(TeamAbbr::BUF),
            fumbles_lost: 3,
            ..factories::player_stats(gsis, 1)
        })
        .collect();
    players.push(NflPlayerWeekStats {
        opponent: Some(TeamAbbr::BUF),
        ..factories::rb_stats("00-B", 1, 100)
    });
    let kc = NflTeamWeekStats {
        game_id: SLATE_GAME.into(),
        points_allowed: 40,
        ..factories::defense_stats("KC", "BUF", 1)
    };
    let buf = NflTeamWeekStats {
        game_id: SLATE_GAME.into(),
        ..factories::scoreless_defense_stats("BUF", "KC", 1)
    };
    app.nfl
        .seed_week_stats_for_test(Season(2025), &players, &[kc, buf])
        .await
        .unwrap();
}

#[sqlx::test]
async fn stored_results_reach_every_surface(pool: SqlitePool) {
    let (app, _mock) = app_at(&pool, KICKOFF).await;
    let a = factories::team(
        &pool,
        TeamOptions {
            name: Some("Gridiron Giants".into()),
            ..Default::default()
        },
    )
    .await;
    let b = factories::team(
        &pool,
        TeamOptions {
            league: Some(a.league.clone()),
            name: Some("Turf Titans".into()),
            ..Default::default()
        },
    )
    .await;

    let slate = publish_slate(&app, week1(), &[scheduled_game()], &stored_lineup_players()).await;
    let alpha = factories::full_entry(&pool, slate, a.team.id, "00-A").await;
    let beta = factories::full_entry(&pool, slate, b.team.id, "00-B").await;
    seed_stored_results(&app, 150, 1, 200, 0, true).await;
    upload_slate(&app, week1(), &[scheduled_game()], &stored_lineup_players()).await;

    app.login_as(&a.owner).await;
    let body = app.get(&format!("/entries/{alpha}")).await.text();
    assert_eq!(
        entry_total(&body, alpha),
        "41.00",
        "150 rushing yards, a TD, the QB's 200 yards, and the DST"
    );
    assert_eq!(row_points(&body, alpha, "rb1"), "24.00");
    assert_eq!(
        breakdown_points(&body, alpha, "rb1"),
        ["15.00", "6.00", "3.00"].map(String::from)
    );
    assert_eq!(row_points(&body, alpha, "qb"), "8.00");
    assert_eq!(row_points(&body, alpha, "def"), "9.00");
    assert_eq!(
        breakdown_points(&body, alpha, "def"),
        ["3.00", "2.00", "4.00"].map(String::from)
    );
    assert_eq!(row_points(&body, alpha, "rb2"), "0.00");
    assert!(breakdown_points(&body, alpha, "rb2").is_empty());
    assert!(
        body.contains(r#"<div class="w-6 text-center text-xl font-bold tabular-nums">1</div>"#),
        "the top score takes rank 1: {body}"
    );

    let details = app.get(&format!("/contests/{}", slate.0)).await.text();
    assert_eq!(contest_entry_score(&details, slate, alpha), "41.00");
    assert_eq!(contest_entry_score(&details, slate, beta), "30.00");
    let alpha_at = details
        .find(&format!("contest-{}-entry-{alpha}-score", slate.0))
        .expect("alpha's row");
    let beta_at = details
        .find(&format!("contest-{}-entry-{beta}-score", slate.0))
        .expect("beta's row");
    assert!(
        alpha_at < beta_at,
        "the higher total ranks first: {details}"
    );
    assert!(
        details[alpha_at..beta_at].contains("ph-trophy"),
        "the winner takes the trophy"
    );

    let list = app.get("/contests").await.text();
    let row = contest_list_row(&list, slate);
    assert!(row.contains(&a.team.name), "alpha wins: {row}");
    assert!(row.contains("41.00"), "with the stored total: {row}");
    assert!(!row.contains("30.00"), "and only the winner: {row}");

    let standings = standings_service::standings_for_league(&app.state(), a.league.id)
        .await
        .unwrap();
    assert_eq!(standings[0].name, a.team.name);
    assert_eq!(standings[0].total, Some(41.0));
    assert_eq!(standings[0].rank, Some(1));
    assert_eq!(standings[1].name, b.team.name);
    assert_eq!(standings[1].total, Some(30.0));

    let team_page = app.get(&format!("/teams/{}", a.team.id)).await.text();
    assert_eq!(team_row_points(&team_page, slate), "41.00");
    let beta_page = app.get(&format!("/teams/{}", b.team.id)).await.text();
    assert_eq!(team_row_points(&beta_page, slate), "30.00");
}

#[sqlx::test]
async fn held_snapshot_keeps_its_results_across_record_changes(pool: SqlitePool) {
    use crate::live::ContestScores;
    use crate::scoring::service as scoring_service;

    let (app, _mock) = app_at(&pool, KICKOFF).await;
    let a = factories::team(
        &pool,
        TeamOptions {
            name: Some("Gridiron Giants".into()),
            ..Default::default()
        },
    )
    .await;
    let b = factories::team(
        &pool,
        TeamOptions {
            league: Some(a.league.clone()),
            name: Some("Turf Titans".into()),
            ..Default::default()
        },
    )
    .await;

    let slate = publish_slate(&app, week1(), &[scheduled_game()], &stored_lineup_players()).await;
    let alpha = factories::full_entry(&pool, slate, a.team.id, "00-A").await;
    let beta = factories::full_entry(&pool, slate, b.team.id, "00-B").await;
    seed_stored_results(&app, 150, 1, 200, 0, true).await;
    upload_slate(&app, week1(), &[scheduled_game()], &stored_lineup_players()).await;
    let alpha_side = |points: &'static str, rb1: &'static str| Surface {
        team: a.team.id,
        name: &a.team.name,
        entry: alpha,
        points,
        rb1,
    };
    let beta_side = |points: &'static str, rb1: &'static str| Surface {
        team: b.team.id,
        name: &b.team.name,
        entry: beta,
        points,
        rb1,
    };

    let held = app.nfl.snapshot();
    assert!(held.stats_available(&slate));
    assert_eq!(
        scoring_service::lineup_total(&stored_lineup("00-A"), &held, slate),
        41.0
    );
    app.login_as(&a.owner).await;
    assert_stored_surfaces(
        &app,
        slate,
        a.league.id,
        alpha_side("41.00", "24.00"),
        beta_side("30.00", "13.00"),
    )
    .await;

    seed_stored_results(&app, 50, 0, 200, 0, true).await;
    assert!(
        Arc::ptr_eq(&held, &app.nfl.snapshot()),
        "seeding alone publishes nothing"
    );
    assert_eq!(
        scoring_service::lineup_total(&stored_lineup("00-A"), &held, slate),
        41.0
    );
    assert_stored_surfaces(
        &app,
        slate,
        a.league.id,
        alpha_side("41.00", "24.00"),
        beta_side("30.00", "13.00"),
    )
    .await;

    upload_slate(&app, week1(), &[scheduled_game()], &stored_lineup_players()).await;
    assert_stored_surfaces(
        &app,
        slate,
        a.league.id,
        alpha_side("22.00", "5.00"),
        beta_side("30.00", "13.00"),
    )
    .await;
    assert!(held.stats_available(&slate));
    assert_eq!(
        scoring_service::lineup_total(&stored_lineup("00-A"), &held, slate),
        41.0
    );

    seed_stored_results(&app, 50, 0, 0, 2, true).await;
    upload_slate(&app, week1(), &[scheduled_game()], &stored_lineup_players()).await;
    let body = app.get(&format!("/entries/{alpha}")).await.text();
    assert_eq!(row_points(&body, alpha, "qb"), "-2.00", "two interceptions");
    assert_eq!(entry_total(&body, alpha), "12.00", "5 + -2 + 9: {body}");

    seed_negative_week(&app).await;
    upload_slate(&app, week1(), &[scheduled_game()], &stored_lineup_players()).await;
    assert_stored_surfaces(
        &app,
        slate,
        a.league.id,
        alpha_side("-52.00", "-6.00"),
        beta_side("-33.00", "13.00"),
    )
    .await;
    let body = app.get(&format!("/entries/{alpha}")).await.text();
    assert_eq!(
        breakdown_points(&body, alpha, "rb1"),
        ["-6.00"].map(String::from),
        "three lost fumbles at -2.00 each: {body}"
    );
    assert_eq!(
        scoring_service::lineup_total(&stored_lineup("00-A"), &held, slate),
        41.0
    );

    let published = app.nfl.snapshot();
    seed_stored_results(&app, 150, 1, 200, 0, false).await;
    assert!(
        Arc::ptr_eq(&published, &app.nfl.snapshot()),
        "the unpublished change leaves availability alone"
    );
    assert!(matches!(
        live::load_contest_scores(&app.state(), slate)
            .await
            .unwrap(),
        ContestScores::Official(_)
    ));

    upload_slate(&app, week1(), &[scheduled_game()], &stored_lineup_players()).await;
    assert!(matches!(
        live::load_contest_scores(&app.state(), slate)
            .await
            .unwrap(),
        ContestScores::Unavailable
    ));
    let list = app.get("/contests").await.text();
    let row = contest_list_row(&list, slate);
    assert!(
        !row.contains("41.00") && !row.contains("30.00") && !row.contains("ph-trophy"),
        "no partial total is published as a result: {row}"
    );
    let details = app.get(&format!("/contests/{}", slate.0)).await.text();
    assert_eq!(
        contest_entry_score(&details, slate, alpha),
        "",
        "an unavailable slate shows no score"
    );

    seed_stored_results(&app, 150, 1, 200, 0, true).await;
    upload_slate(&app, week1(), &[scheduled_game()], &stored_lineup_players()).await;
    assert!(matches!(
        live::load_contest_scores(&app.state(), slate)
            .await
            .unwrap(),
        ContestScores::Official(_)
    ));
    assert_stored_surfaces(
        &app,
        slate,
        a.league.id,
        alpha_side("41.00", "24.00"),
        beta_side("30.00", "13.00"),
    )
    .await;
}
