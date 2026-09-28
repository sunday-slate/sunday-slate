//! Slate publishing: the admin editor at `/admin/contests/{id}/slate`.

use http::StatusCode;
use nfl_data::{Game, Season, SeasonType, TeamAbbr as NflTeamAbbr, Week};
use sqlx::SqlitePool;
use time::OffsetDateTime;
use time::macros::datetime;

use crate::tests::TestApp;
use crate::tests::factories::{self, UserOptions};
use crate::tests::utils::form_body;

pub(crate) fn game(id: &str, week: u8, kickoff: OffsetDateTime) -> Game {
    Game {
        gsis_game_id: id.into(),
        season: Season(2025),
        week: Week(week),
        season_type: SeasonType::Reg,
        kickoff: Some(kickoff),
        away_team: NflTeamAbbr("BUF".into()),
        home_team: NflTeamAbbr("KC".into()),
        home_score: None,
        away_score: None,
    }
}

/// Week 1, 2025: a Thursday opener, a Sunday afternoon game, and Sunday night.
/// `resolve` proposes only the Sunday afternoon game. Each game carries a
/// distinct matchup, so an assertion on rendered text names one game.
pub(crate) fn week_one() -> Vec<Game> {
    vec![
        Game {
            away_team: NflTeamAbbr("DAL".into()),
            home_team: NflTeamAbbr("PHI".into()),
            ..game("2025_01_THU", 1, datetime!(2025-09-05 00:20 UTC))
        },
        game("2025_01_BUF_KC", 1, datetime!(2025-09-07 17:00 UTC)),
        Game {
            away_team: NflTeamAbbr("SF".into()),
            home_team: NflTeamAbbr("LA".into()),
            ..game("2025_01_SNF", 1, datetime!(2025-09-08 00:20 UTC))
        },
    ]
}

/// Whether the checkbox for `game_id` is rendered checked. Inspects that one
/// input's own attributes, so a checked box elsewhere cannot satisfy it.
fn box_checked(html: &str, game_id: &str) -> bool {
    html.split("<input")
        .filter_map(|tag| tag.split_once('>').map(|(open, _)| open))
        .find(|open| open.contains(&format!(r#"name="game_{game_id}""#)))
        .is_some_and(|open| open.contains("checked"))
}

#[sqlx::test]
async fn the_editor_checks_the_formulas_proposal_on_a_first_visit(pool: SqlitePool) {
    let app = TestApp::from_pool_at(pool.clone(), datetime!(2025-09-01 00:00 UTC)).await;
    app.nfl.seed_for_test(&[], &week_one()).await.unwrap();
    let wk1 = factories::contest(&pool, "Week 1").await;
    app.login_admin().await;

    let page = app.get(&format!("/admin/contests/{wk1}/slate")).await;
    page.assert_status_ok();
    let html = page.text();
    // The whole week is offered.
    assert!(html.contains(r#"name="game_2025_01_THU""#));
    assert!(html.contains(r#"name="game_2025_01_BUF_KC""#));
    assert!(html.contains(r#"name="game_2025_01_SNF""#));
    // Only the formula's pick starts checked.
    assert!(box_checked(&html, "2025_01_BUF_KC"));
    assert!(!box_checked(&html, "2025_01_THU"));
    assert!(!box_checked(&html, "2025_01_SNF"));
    assert!(html.contains("Publish slate"));
}

#[sqlx::test]
async fn the_editor_closes_to_the_contest_list(pool: SqlitePool) {
    let app = TestApp::from_pool_at(pool.clone(), datetime!(2025-09-01 00:00 UTC)).await;
    app.nfl.seed_for_test(&[], &week_one()).await.unwrap();
    let wk1 = factories::contest(&pool, "Week 1").await;
    app.login_admin().await;

    let page = app.get(&format!("/admin/contests/{wk1}/slate")).await;
    page.assert_status_ok();
    let html = page.text();
    assert!(
        html.contains(
            r#"href="/admin/contests" class="btn btn-ghost btn-square -ml-2" aria-label="Close""#
        ),
        "editor should close out to the contest list: {html}"
    );
    assert!(
        html.contains(">Week 1 slate</span>"),
        "bar should name the contest whose slate this is: {html}"
    );
}

#[sqlx::test]
async fn the_editor_checks_the_published_slate_on_a_return_visit(pool: SqlitePool) {
    let app = TestApp::from_pool_at(pool.clone(), datetime!(2025-09-01 00:00 UTC)).await;
    app.nfl.seed_for_test(&[], &week_one()).await.unwrap();
    let wk1 = factories::contest_with_games(&pool, "Week 1", &["2025_01_THU"]).await;
    app.login_admin().await;

    let page = app.get(&format!("/admin/contests/{wk1}/slate")).await;
    page.assert_status_ok();
    let html = page.text();
    assert!(box_checked(&html, "2025_01_THU"));
    assert!(!box_checked(&html, "2025_01_BUF_KC"));
    assert!(!box_checked(&html, "2025_01_SNF"));
    assert!(html.contains("Update slate"));
}

#[sqlx::test]
async fn a_started_slate_renders_without_checkboxes(pool: SqlitePool) {
    // `now` is after the Thursday kickoff.
    let app = TestApp::from_pool_at(pool.clone(), datetime!(2025-09-05 02:00 UTC)).await;
    app.nfl.seed_for_test(&[], &week_one()).await.unwrap();
    let wk1 = factories::contest_with_games(&pool, "Week 1", &["2025_01_THU"]).await;
    app.login_admin().await;

    let page = app.get(&format!("/admin/contests/{wk1}/slate")).await;
    page.assert_status_ok();
    let html = page.text();
    assert!(!html.contains("<input type=\"checkbox\""), "no checkboxes");
    assert!(html.contains("These games have started."));
    assert!(html.contains("DAL @ PHI"));
}

#[sqlx::test]
async fn a_contest_that_does_not_exist_is_404(pool: SqlitePool) {
    let app = TestApp::from_pool(pool.clone()).await;
    app.login_admin().await;
    app.get("/admin/contests/999/slate")
        .await
        .assert_status(StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn a_non_admin_cannot_reach_the_editor(pool: SqlitePool) {
    let app = TestApp::from_pool(pool.clone()).await;
    let wk1 = factories::contest(&pool, "Week 1").await;
    let user = factories::user(&pool, UserOptions::default()).await;
    app.login_as(&user.user).await;
    app.get(&format!("/admin/contests/{wk1}/slate"))
        .await
        .assert_status(StatusCode::FORBIDDEN);
}

async fn published(pool: &SqlitePool, contest_id: i64) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT gsis_game_id FROM contest_games WHERE contest_id = ?1 ORDER BY gsis_game_id",
    )
    .bind(contest_id)
    .fetch_all(pool)
    .await
    .unwrap()
}

#[sqlx::test]
async fn publishing_replaces_the_slate(pool: SqlitePool) {
    let app = TestApp::from_pool_at(pool.clone(), datetime!(2025-09-01 00:00 UTC)).await;
    app.nfl.seed_for_test(&[], &week_one()).await.unwrap();
    let wk1 = factories::contest_with_games(&pool, "Week 1", &["2025_01_THU"]).await;
    app.login_admin().await;

    let resp = app
        .post(
            &format!("/admin/contests/{wk1}/slate"),
            &form_body(&[("game_2025_01_BUF_KC", "on"), ("game_2025_01_SNF", "on")]),
        )
        .await;
    resp.assert_status(StatusCode::SEE_OTHER);
    assert_eq!(resp.header("location"), "/admin/contests");

    // The Thursday game is gone; the two checked games are in.
    assert_eq!(
        published(&pool, wk1).await,
        vec!["2025_01_BUF_KC".to_string(), "2025_01_SNF".to_string()]
    );
}

#[sqlx::test]
async fn an_empty_selection_is_refused(pool: SqlitePool) {
    let app = TestApp::from_pool_at(pool.clone(), datetime!(2025-09-01 00:00 UTC)).await;
    app.nfl.seed_for_test(&[], &week_one()).await.unwrap();
    let wk1 = factories::contest_with_games(&pool, "Week 1", &["2025_01_THU"]).await;
    app.login_admin().await;

    let resp = app.post(&format!("/admin/contests/{wk1}/slate"), "").await;
    resp.assert_status(StatusCode::SEE_OTHER);
    assert_eq!(
        resp.header("location"),
        format!("/admin/contests/{wk1}/slate")
    );
    assert_eq!(published(&pool, wk1).await, vec!["2025_01_THU".to_string()]);

    // The message rides the session to the next page.
    let page = app.get(&format!("/admin/contests/{wk1}/slate")).await;
    assert!(page.text().contains("Select at least one game."));
}

#[sqlx::test]
async fn a_game_outside_the_week_is_refused(pool: SqlitePool) {
    let app = TestApp::from_pool_at(pool.clone(), datetime!(2025-09-01 00:00 UTC)).await;
    app.nfl.seed_for_test(&[], &week_one()).await.unwrap();
    let wk1 = factories::contest(&pool, "Week 1").await;
    app.login_admin().await;

    app.post(
        &format!("/admin/contests/{wk1}/slate"),
        &form_body(&[("game_2025_09_ELSEWHERE", "on")]),
    )
    .await;
    assert!(published(&pool, wk1).await.is_empty());
}

#[sqlx::test]
async fn a_started_slate_refuses_the_post(pool: SqlitePool) {
    let app = TestApp::from_pool_at(pool.clone(), datetime!(2025-09-05 02:00 UTC)).await;
    app.nfl.seed_for_test(&[], &week_one()).await.unwrap();
    let wk1 = factories::contest_with_games(&pool, "Week 1", &["2025_01_THU"]).await;
    app.login_admin().await;

    app.post(
        &format!("/admin/contests/{wk1}/slate"),
        &form_body(&[("game_2025_01_SNF", "on")]),
    )
    .await;
    assert_eq!(published(&pool, wk1).await, vec!["2025_01_THU".to_string()]);

    let page = app.get(&format!("/admin/contests/{wk1}/slate")).await;
    assert!(page.text().contains("These games have started."));
}

#[sqlx::test]
async fn the_index_links_each_contest_to_its_editor(pool: SqlitePool) {
    let app = TestApp::from_pool_at(pool.clone(), datetime!(2025-09-01 00:00 UTC)).await;
    app.nfl.seed_for_test(&[], &week_one()).await.unwrap();
    let wk1 = factories::contest(&pool, "Week 1").await;
    app.login_admin().await;

    let html = app.get("/admin/contests").await.text();
    assert!(html.contains(&format!(r#"href="/admin/contests/{wk1}/slate""#)));
}

/// The whole flow, reaching each step only through rendered output: the admin
/// menu links to contests, a contest card links to its slate editor, the editor
/// posts to its own action, and a league member then sees those games.
#[sqlx::test]
async fn a_commissioner_publishes_a_slate_that_members_then_see(pool: SqlitePool) {
    let app = TestApp::from_pool_at(pool.clone(), datetime!(2025-09-01 00:00 UTC)).await;
    app.nfl.seed_for_test(&[], &week_one()).await.unwrap();
    let member = factories::team(&pool, factories::TeamOptions::default()).await;

    // An admin who is also this league's member, so one identity walks the flow.
    sqlx::query("UPDATE users SET is_admin = 1 WHERE id = ?1")
        .bind(member.owner.id)
        .execute(&pool)
        .await
        .unwrap();
    app.login_as(&member.owner).await;

    // The season's contests come from the setup form, as they do in production.
    let admin_home = app.get("/admin").await;
    admin_home.assert_status_ok();
    assert!(admin_home.text().contains(r#"href="/admin/contests""#));

    let empty_list = app.get("/admin/contests").await;
    empty_list.assert_status_ok();
    assert!(
        empty_list
            .text()
            .contains(r#"href="/admin/contests/setup""#)
    );

    let setup = app.get("/admin/contests/setup").await;
    setup.assert_status_ok();
    assert!(setup.text().contains(r#"action="/admin/contests/setup""#));

    app.post("/admin/contests/setup", "").await;

    // A contest card links to its editor.
    let list = app.get("/admin/contests").await;
    list.assert_status_ok();
    let editor_href = list
        .text()
        .split(r#"href=""#)
        .find(|s| s.starts_with("/admin/contests/") && s.contains("/slate"))
        .map(|s| s.split('"').next().unwrap().to_string())
        .expect("a card links to a slate editor");
    assert!(
        editor_href.ends_with("/slate"),
        "the card links to a slate editor, got {editor_href}"
    );

    let editor = app.get(&editor_href).await;
    editor.assert_status_ok();
    let html = editor.text();
    assert!(
        html.contains(&format!(r#"action="{editor_href}""#)),
        "form action"
    );
    assert!(html.contains(r#"name="game_2025_01_BUF_KC""#));

    // Publish the Sunday afternoon game and the Sunday night game.
    let publish = app
        .post(
            &editor_href,
            &form_body(&[("game_2025_01_BUF_KC", "on"), ("game_2025_01_SNF", "on")]),
        )
        .await;
    publish.assert_status(StatusCode::SEE_OTHER);
    assert_eq!(publish.header("location"), "/admin/contests");

    // The list now shows the published matchups, and not the unchecked game.
    let after = app.get("/admin/contests").await;
    assert!(after.text().contains("BUF @ KC"));
    assert!(!after.text().contains("DAL @ PHI"));

    // A member reaches the contest from the Contests tab and sees the games.
    let board = app.get("/standings").await;
    board.assert_status_ok();
    assert!(
        board.text().contains(r#"href="/contests""#),
        "the Contests tab links to the contest list"
    );
    let contests = app.get("/contests").await;
    contests.assert_status_ok();
    let contest_id: i64 = sqlx::query_scalar("SELECT id FROM contests WHERE name = 'Week 1'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(
        contests.text().contains(&format!("/contests/{contest_id}")),
        "the list links to the contest"
    );
    let details = app.get(&format!("/contests/{contest_id}")).await;
    details.assert_status_ok();
    assert!(details.text().contains("BUF @ KC"));
    assert!(details.text().contains("SF @ LA"));
    assert!(
        !details.text().contains("DAL @ PHI"),
        "the unchecked game must not leak onto the contest page"
    );
    assert_eq!(
        published(&pool, contest_id).await,
        vec!["2025_01_BUF_KC".to_string(), "2025_01_SNF".to_string()]
    );
}
