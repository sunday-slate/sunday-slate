use crate::tests::factories::{LeagueOptions, ResetTokenOptions, TeamOptions, UserOptions};
use crate::tests::utils::*;
use crate::tests::{TestApp, factories};

#[tokio::test]
async fn login_with_valid_credentials_then_access_protected_page() {
    let app = TestApp::new().await;
    let gen_user = factories::user(&app.pool, UserOptions::default()).await;
    let gen_team = factories::team(
        &app.pool,
        TeamOptions {
            owner: Some(gen_user.user.clone()),
            ..Default::default()
        },
    )
    .await;

    app.login(&gen_user).await;

    let resp = app.home().await;

    let body_str = resp.text();
    assert!(
        body_str.contains(&as_rendered(&gen_team.league.name)),
        "missing league brand: {body_str}"
    );
}

/// Password reset flash: shows on first login after reset, gone on reload.
#[tokio::test]
async fn login_shows_reset_flash_once() {
    let app = TestApp::new().await;
    let gen_reset = factories::reset_token(&app.pool, ResetTokenOptions::default()).await;
    let token = gen_reset.token();

    let resp = app
        .post(
            "/reset-password",
            &form_body(&[
                ("token", &token),
                ("password", "newpass123"),
                ("password_confirm", "newpass123"),
            ]),
        )
        .await;

    assert!(resp.status_code().is_redirection());
    assert_eq!(resp.header("location"), "/login");

    // The jar carries the flash cookie from the redirect into the next request.
    let login = app.get("/login").await;
    login.assert_status_ok();
    let body = login.text();
    assert!(
        body.contains("Your password has been reset"),
        "missing reset success flash: {body}"
    );

    // One-shot: the flash must not survive a reload.
    let reload = app.get("/login").await;
    reload.assert_status_ok();
    let body = reload.text();
    assert!(
        !body.contains("Your password has been reset"),
        "flash must not survive a reload: {body}"
    );
}

/// Full journey: signed-in user sees the menu, then sign-out ends the session.
#[tokio::test]
async fn home_menu_signs_user_out() {
    let app = TestApp::new().await;
    let gen_league = factories::league(&app.pool, LeagueOptions::default()).await;
    let user = gen_league.commish;

    app.login_as(&user).await;

    let home = app.home().await;
    let body = home.text();
    assert!(
        body.contains(r#"aria-label="Menu""#),
        "no overflow menu on home: {body}"
    );
    assert!(
        body.contains(r#"hx-post="/logout""#),
        "no htmx sign out on home: {body}"
    );

    // Signing out returns the htmx redirect and clears the session cookie; the jar
    // picks up the cleared cookie so the next request is unauthenticated.
    let logout = app.post_htmx("/logout", "").await;
    assert_eq!(logout.header("hx-redirect"), "/login");

    let after = app.get("/").await;
    assert!(
        after.status_code().is_redirection(),
        "home should redirect to login after logout, got {}",
        after.status_code()
    );
}
