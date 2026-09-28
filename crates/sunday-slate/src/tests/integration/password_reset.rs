use crate::Db;
use crate::tests::factories::{ResetTokenOptions, TeamOptions, UserOptions};
use crate::tests::utils::*;
use crate::tests::{TestApp, factories};
use fake::Fake;
use fake::faker::internet::en::Password;

/// GET /forgot-password renders the email form. After a POST, the session
/// carries a one-shot "Check your email" flash: it shows on the next GET and
/// is gone on a reload.
#[tokio::test]
async fn forgot_password_form_renders() {
    let app = TestApp::new().await;
    factories::user(&app.pool, UserOptions::default()).await;
    let form = app.get("/forgot-password").await;
    form.assert_status_ok();
    let body = form.text();
    assert!(
        body.contains(r#"name="email""#),
        "missing email input: {body}"
    );

    let post = app
        .post(
            "/forgot-password",
            &form_body(&[("email", "nobody@example.com")]),
        )
        .await;
    assert!(post.status_code().is_redirection());

    // The jar carries the flash cookie from the redirect into the next request.
    let sent = app.get("/forgot-password").await;
    sent.assert_status_ok();
    let body = sent.text();
    assert!(
        body.contains("Check your email"),
        "missing confirmation flash: {body}"
    );
    assert!(
        body.contains("alert-success"),
        "confirmation flash should render as a success alert: {body}"
    );

    // One-shot: a reload shows the plain form again.
    let reload = app.get("/forgot-password").await;
    reload.assert_status_ok();
    let body = reload.text();
    assert!(
        !body.contains("Check your email"),
        "flash must not survive a reload: {body}"
    );
}

/// Requesting a new link upserts the user's single row (still one row) and
/// kills the previous link: the earlier token no longer validates.
#[tokio::test]
async fn reset_get_token_invalidated_by_new_request() {
    let app = TestApp::new().await;
    let gen_reset = factories::reset_token(&app.pool, ResetTokenOptions::default()).await;
    let old_token = gen_reset.token();

    let db = Db::test(app.pool.clone());

    crate::auth::reset::send_reset_email(
        &db,
        &app.mailer,
        "http://localhost:3000",
        &gen_reset.user.email,
    )
    .await
    .expect("send_reset_email");

    let token_count = crate::auth::store::token_count(&app.pool)
        .await
        .expect("token count");
    assert_eq!(token_count, 1, "still exactly one row per user");

    let stale = app.get(&format!("/reset-password?token={old_token}")).await;
    assert!(
        stale.status_code().is_redirection(),
        "old token should be invalidated by the new request, got {}",
        stale.status_code()
    );
    assert_eq!(stale.header("location"), "/forgot-password");
}

/// A valid submission resets the password, consumes the token (single use),
/// and redirects to the login page (the success flash rides the session).
#[tokio::test]
async fn reset_post_success_then_single_use() {
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

    // Token cannot be replayed (single use) — a still-anonymous GET redirects to
    // forgot-password. Checked before signing in, so the request carries no auth
    // (an authenticated user would instead be bounced to their home route).
    let replay = app.get(&format!("/reset-password?token={token}")).await;
    assert!(
        replay.status_code().is_redirection(),
        "consumed token should be invalid, got {}",
        replay.status_code()
    );
    assert_eq!(replay.header("location"), "/forgot-password");

    // New password works.
    let login_resp = app
        .post(
            "/login",
            &form_body(&[("email", &gen_reset.user.email), ("password", "newpass123")]),
        )
        .await;
    let cookie = login_resp.header("set-cookie");
    let cookie = cookie.to_str().unwrap();
    assert!(cookie.contains("id="), "new password should authenticate");
}

/// Session established before a reset stops authenticating afterward because
/// the changed password hash invalidates the stored session auth hash.
#[tokio::test]
async fn reset_invalidates_existing_sessions() {
    let app = TestApp::new().await;
    let gen_team = factories::team(&app.pool, TeamOptions::default()).await;

    // Log in to establish a live session (carried by the jar), and confirm it works.
    app.login_as(&gen_team.owner).await;
    app.get("/standings").await.assert_status_ok();

    // Reset the password via a valid token.
    let gen_reset = factories::reset_token(
        &app.pool,
        ResetTokenOptions {
            user: Some(gen_team.owner),
        },
    )
    .await;

    let token = gen_reset.token();
    let pass: String = Password(8..16).fake();
    let reset = app
        .post(
            "/reset-password",
            &form_body(&[
                ("token", &token),
                ("password", &pass),
                ("password_confirm", &pass),
            ]),
        )
        .await;
    assert!(reset.status_code().is_redirection());

    // The pre-reset session no longer authenticates — protected route redirects to login.
    let after = app.get("/").await;
    assert!(
        after.status_code().is_redirection(),
        "old session must be invalid after reset, got {}",
        after.status_code()
    );
}

/// Full journey from login page: forgot-password link → form → emailed token
/// → set new password → sign in. Follows only links/URLs from rendered output.
#[tokio::test]
async fn full_reset_journey_starting_from_login_page() {
    let app = TestApp::new().await;
    let gen_user = factories::user(&app.pool, UserOptions::default()).await;

    // 1. Login page links to forgot-password.
    let login_page = app.get("/login").await;
    login_page.assert_status_ok();
    let body = login_page.text();
    assert!(
        body.contains(r#"href="/forgot-password""#),
        "login page must link to /forgot-password: {body}"
    );

    // 2. Follow link to forgot-password form.
    let forgot_page = app.get("/forgot-password").await;
    forgot_page.assert_status_ok();
    let body = forgot_page.text();
    assert!(
        body.contains(r#"action="/forgot-password""#),
        "missing form: {body}"
    );
    assert!(
        body.contains(r#"name="email""#),
        "missing email input: {body}"
    );

    // 3. Submit the email address.
    let post = app
        .post(
            "/forgot-password",
            &form_body(&[("email", &gen_user.user.email)]),
        )
        .await;
    assert!(post.status_code().is_redirection());

    // 4. Poll for the detached email task.
    let mut emails = app.mailer.all();
    for _ in 0..200 {
        if !emails.is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        emails = app.mailer.all();
    }
    assert_eq!(
        emails.len(),
        1,
        "reset email should arrive from the detached task"
    );

    // 5. Extract the reset link from the email.
    let link = emails[0]
        .text
        .split_whitespace()
        .find(|w| w.starts_with("http://localhost:3000/reset-password?token="))
        .expect("reset link in email text body");
    let path = link.strip_prefix("http://localhost:3000").unwrap();
    let token = link.split("token=").nth(1).unwrap();

    let reset_page = app.get(path).await;
    reset_page.assert_status_ok();
    let body = reset_page.text();
    assert!(
        body.contains("Set a new password"),
        "emailed link must open the reset form: {body}"
    );
    assert!(
        body.contains(&format!(r#"value="{token}""#)),
        "form must carry the token from the link: {body}"
    );

    // 6. Submit the new password.
    let reset = app
        .post(
            "/reset-password",
            &form_body(&[
                ("token", token),
                ("password", "newpass123"),
                ("password_confirm", "newpass123"),
            ]),
        )
        .await;
    assert!(reset.status_code().is_redirection());
    assert_eq!(reset.header("location"), "/login");

    // 7. Sign in with the new password.
    let login_resp = app
        .post(
            "/login",
            &form_body(&[("email", "alice@example.com"), ("password", "newpass123")]),
        )
        .await;
    let cookie = login_resp.header("set-cookie");
    let cookie = cookie.to_str().unwrap();
    assert!(cookie.contains("id="), "new password should authenticate");
}
