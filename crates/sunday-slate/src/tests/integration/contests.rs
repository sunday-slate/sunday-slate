use crate::tests::factories::UserOptions;
use crate::tests::utils::*;
use crate::tests::{TestApp, factories};
use http::StatusCode;

#[tokio::test]
async fn admin_sets_up_contests_via_toggles() {
    let app = TestApp::new().await;
    app.login_admin().await;

    // Admin Tools landing links to the contests page.
    let admin_home = app.get("/admin").await;
    admin_home.assert_status_ok();
    assert!(
        admin_home.text().contains(r#"href="/admin/contests""#),
        "admin menu links to contests"
    );

    // Contests page links to the setup form.
    let list = app.get("/admin/contests").await;
    list.assert_status_ok();
    assert!(
        list.text().contains(r#"href="/admin/contests/setup""#),
        "setup link"
    );

    let setup = app.get("/admin/contests/setup").await;
    setup.assert_status_ok();
    assert!(
        setup.text().contains(r#"action="/admin/contests/setup""#),
        "setup form"
    );

    // Toggle Thanksgiving + Wild Card on; leave the rest off.
    let submit = app
        .post(
            "/admin/contests/setup",
            &form_body(&[("thanksgiving", "on"), ("wild_card", "on")]),
        )
        .await;
    submit.assert_status(StatusCode::SEE_OTHER);
    assert_eq!(submit.header("location"), "/admin/contests");

    // 18 weeks + Thanksgiving + Wild Card = 20 contests; Christmas absent.
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM contests")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(count, 20);
    let body = app.get("/admin/contests").await.text();
    assert!(body.contains("Week 1"), "week shown");
    assert!(body.contains("Thanksgiving"), "thanksgiving shown");
    assert!(body.contains("Wild Card"), "wild card shown");
    assert!(!body.contains("Christmas"), "christmas not shown");
    // Setup is one-time, so the list stops offering it.
    assert!(
        !body.contains(r#"href="/admin/contests/setup""#),
        "setup link gone once configured"
    );
}

#[tokio::test]
async fn setup_form_defaults_all_optionals_on_when_unconfigured() {
    let app = TestApp::new().await;
    app.login_admin().await;
    // Fresh install, no contests yet: every optional toggle defaults to checked.
    let body = app.get("/admin/contests/setup").await.text();
    assert_eq!(
        body.matches("checked").count(),
        5,
        "all five optionals default on before setup"
    );
}

#[tokio::test]
async fn setup_form_redirects_once_configured() {
    let app = TestApp::new().await;
    app.login_admin().await;
    app.post("/admin/contests/setup", &form_body(&[("christmas", "on")]))
        .await;

    // The contest list is fixed, so there is no form left to show.
    let form = app.get("/admin/contests/setup").await;
    form.assert_status(StatusCode::SEE_OTHER);
    assert_eq!(form.header("location"), "/admin/contests");
}

#[tokio::test]
async fn resubmitting_setup_cannot_change_the_contest_list() {
    let app = TestApp::new().await;
    app.login_admin().await;
    app.post("/admin/contests/setup", &form_body(&[("christmas", "on")]))
        .await;

    // A team submits an entry. Removing the contest would cascade it away, which
    // is why the list is fixed after setup.
    let team = factories::team(&app.pool, factories::TeamOptions::default()).await;
    let contest_id: i64 = sqlx::query_scalar("SELECT id FROM contests WHERE name = 'Christmas'")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO entries (contest_id, fantasy_team_id) VALUES (?1, ?2)")
        .bind(contest_id)
        .bind(team.team.id)
        .execute(&app.pool)
        .await
        .unwrap();

    // A stale tab resubmits: nothing checked, plus an optional that was off.
    app.post("/admin/contests/setup", &form_body(&[("wild_card", "on")]))
        .await;

    let names: Vec<String> = sqlx::query_scalar("SELECT name FROM contests ORDER BY id")
        .fetch_all(&app.pool)
        .await
        .unwrap();
    assert_eq!(names.len(), 19, "18 weeks + Christmas, unchanged");
    assert!(names.contains(&"Christmas".to_string()), "nothing removed");
    assert!(!names.contains(&"Wild Card".to_string()), "nothing added");
    let entries: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM entries")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(entries, 1, "entry untouched");
}

#[tokio::test]
async fn non_admin_cannot_reach_contests() {
    let app = TestApp::new().await;
    // A logged-in user who is not a site admin.
    let user = factories::user(&app.pool, UserOptions::default()).await;
    app.login(&user).await;
    app.get("/admin/contests")
        .await
        .assert_status(StatusCode::FORBIDDEN);
}
