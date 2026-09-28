use crate::tests::factories::{TeamOptions, UserOptions};
use crate::tests::utils::*;
use crate::tests::{TestApp, factories};
use http::StatusCode;

#[tokio::test]
async fn authed_user_without_league_is_redirected_to_leagues_new() {
    let app = TestApp::new().await;
    let gen_user = factories::user(&app.pool, UserOptions::default()).await;
    app.login(&gen_user).await;

    let resp = app.get("/").await;

    resp.assert_status(StatusCode::SEE_OTHER);
    assert_eq!(resp.header("location"), "/leagues/new");
}

#[tokio::test]
async fn authed_user_with_league_but_no_team_is_redirected_to_teams_new() {
    let app = TestApp::new().await;
    let gen_user = factories::user(
        &app.pool,
        UserOptions {
            is_admin: true,
            ..Default::default()
        },
    )
    .await;

    app.login(&gen_user).await;

    let create = app
        .post("/leagues", &form_body(&[("name", "Sunday Funday")]))
        .await;
    create.assert_status(StatusCode::SEE_OTHER);
    assert_eq!(create.header("location"), "/teams/new");

    let resp = app.get("/").await;
    resp.assert_status(StatusCode::SEE_OTHER);
    assert_eq!(resp.header("location"), "/teams/new");
}

#[tokio::test]
async fn non_admin_cannot_reach_leagues_new() {
    let app = TestApp::new().await;
    let user = factories::user(&app.pool, UserOptions::default()).await;
    app.login(&user).await;

    app.get("/leagues/new")
        .await
        .assert_status(StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn full_bootstrap_journey_from_fresh_install() {
    let app = TestApp::new().await;

    // 1. Root on a fresh install → /setup
    let r = app.get("/").await;
    assert_eq!(r.header("location"), "/setup");

    // 2. Create the first user (commissioner) via /setup. The jar carries the
    //    session it creates through the rest of the journey.
    let r = app
        .post(
            "/setup",
            &form_body(&[
                ("email", "commish@example.com"),
                ("password", "hunter22"),
                ("password_confirm", "hunter22"),
            ]),
        )
        .await;
    assert_eq!(r.header("location"), "/");

    // 3. Root now → /leagues/new (authed, no league)
    let r = app.get("/").await;
    assert_eq!(r.header("location"), "/leagues/new");

    // 4. Follow to the league form and confirm it renders
    let r = app.get("/leagues/new").await;
    r.assert_status_ok();
    let body = r.text();
    assert!(body.contains(r#"action="/leagues""#), "league form: {body}");

    // 5. Submit the league
    let r = app
        .post("/leagues", &form_body(&[("name", "Sunday Funday")]))
        .await;
    assert_eq!(r.header("location"), "/teams/new");

    // 6. Root now → /teams/new (league exists, no team)
    let r = app.get("/").await;
    assert_eq!(r.header("location"), "/teams/new");

    // 7. Follow to the team form
    let r = app.get("/teams/new").await;
    r.assert_status_ok();
    let body = r.text();
    assert!(body.contains(r#"action="/teams""#), "team form: {body}");

    // 8. Submit the team
    let r = app
        .post(
            "/teams",
            &form_body(&[("name", "Gridiron Giants"), ("owner_name", "Mike")]),
        )
        .await;
    assert_eq!(r.header("location"), "/");

    // 9. Root now renders the standings with the commissioner's team
    let r = app.home().await;
    let body = r.text();
    assert!(body.contains("Sunday Funday"), "league name: {body}");
    assert!(body.contains("Gridiron Giants"), "team name: {body}");
    assert!(body.contains("Mike"), "owner name: {body}");

    // 10. DB state: one league, one commissioner team
    let teams: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM fantasy_teams")
        .fetch_one(&app.pool)
        .await
        .expect("count teams");
    assert_eq!(teams, 1);
    let is_commissioner: bool =
        sqlx::query_scalar("SELECT is_commissioner FROM fantasy_teams LIMIT 1")
            .fetch_one(&app.pool)
            .await
            .expect("commissioner flag");
    assert!(is_commissioner, "the bootstrap team is the commissioner's");
}

/// Journey: the admin creates a second league from the admin index, lands on
/// /teams/new, creates the commissioner team, and sees the new league's
/// standings at /.
#[tokio::test]
async fn admin_creates_a_second_league_end_to_end() {
    let app = TestApp::new().await;
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
            owner: Some(admin.user.clone()),
            name: Some("First League Team".into()),
            is_commish: true,
            ..Default::default()
        },
    )
    .await;
    app.login(&admin).await;

    let admin_index = app.get("/admin").await;
    admin_index.assert_status_ok();
    assert!(admin_index.text().contains(r#"href="/leagues/new""#));

    app.get("/leagues/new").await.assert_status_ok();
    let created = app
        .post("/leagues", &form_body(&[("name", "Backyard Bowl")]))
        .await;
    created.assert_status(StatusCode::SEE_OTHER);
    assert_eq!(created.header("location"), "/teams/new");

    app.get("/teams/new").await.assert_status_ok();
    let team = app
        .post(
            "/teams",
            &form_body(&[("name", "Bowl Bosses"), ("owner_name", "Jason")]),
        )
        .await;
    team.assert_status(StatusCode::SEE_OTHER);

    let home = app.home().await;
    let html = home.text();
    assert!(html.contains("Backyard Bowl"));
    assert!(html.contains("Bowl Bosses"));
    assert!(!html.contains("First League Team"));
}
