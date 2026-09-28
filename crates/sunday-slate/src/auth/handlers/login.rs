use crate::auth::{Credentials, MaybeUser};
use crate::web::{form_response, redirect};
use crate::{AppError, Db, form_view};
use askama::Template;
use axum::Form;
use axum::extract::Query;
use axum::response::{IntoResponse, Redirect, Response};
use axum_htmx::HxRequest;
use axum_login::AuthSession;
use axum_messages::Messages;
use serde::Deserialize;

#[derive(Deserialize)]
pub struct LoginQuery {
    next: Option<String>,
}

#[derive(Default, Template)]
#[template(path = "auth/login.html", blocks = ["form"])]
struct LoginTemplate {
    error: Option<String>,
    next: Option<String>,
}
form_view!(LoginTemplate);

#[derive(Deserialize)]
pub struct LoginInput {
    email: String,
    password: String,
    next: Option<String>,
}

pub async fn login(
    MaybeUser(user): MaybeUser,
    HxRequest(is_htmx): HxRequest,
    Query(params): Query<LoginQuery>,
) -> Result<impl IntoResponse, AppError> {
    match user {
        Some(_user) => {
            let next = safe_next(params.next);
            Ok(Redirect::to(&next).into_response())
        }
        None => {
            let template = LoginTemplate {
                error: None,
                next: params.next,
            };
            Ok(form_response(is_htmx, &template)?)
        }
    }
}

pub async fn handle_login(
    HxRequest(is_htmx): HxRequest,
    auth_session: AuthSession<Db>,
    Form(form): Form<LoginInput>,
) -> Result<impl IntoResponse, AppError> {
    let next = form.next;
    let creds = Credentials {
        email: form.email,
        password: form.password,
    };

    let Some(user) = auth_session.authenticate(creds).await? else {
        let error = Some("Invalid email or password".to_string());
        let template = LoginTemplate { error, next };
        return form_response(is_htmx, &template);
    };

    auth_session.login(&user).await?;
    Ok(redirect(is_htmx, safe_next(next)))
}

#[derive(Deserialize)]
pub struct LogoutInput {
    next: Option<String>,
}

pub async fn handle_logout(
    HxRequest(is_htmx): HxRequest,
    auth_session: AuthSession<Db>,
    messages: Messages,
    Form(form): Form<LogoutInput>,
) -> Result<Response, AppError> {
    auth_session.logout().await?;
    messages.info("You've been signed out.");
    let next = match form.next {
        Some(n) => safe_next(Some(n)),
        None => "/login".to_string(),
    };
    Ok(redirect(is_htmx, next))
}

fn safe_next(next: Option<String>) -> String {
    next.filter(|n| n.starts_with('/') && !n.starts_with("//") && !n.starts_with("/\\"))
        .unwrap_or_else(|| "/".to_string())
}

/// Test-only: establish a session for `user_id` without the login form.
/// Routed at `/__test/login/{user_id}` behind `#[cfg(test)]` in `auth::router()`.
#[cfg(test)]
pub async fn handle_test_login(
    axum::extract::Path(user_id): axum::extract::Path<i64>,
    auth_session: AuthSession<Db>,
) -> Result<impl IntoResponse, AppError> {
    use axum_login::AuthnBackend;

    let user = auth_session
        .backend()
        .get_user(&user_id)
        .await?
        .expect("test login: user not found");
    auth_session.login(&user).await?;
    Ok(http::StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::factories::UserOptions;
    use crate::tests::utils::*;
    use crate::tests::{TestApp, factories};
    use fake::Fake;
    use fake::faker::internet::en::SafeEmail;
    use http::StatusCode;

    #[tokio::test]
    async fn login_page_returns_200() {
        let app = TestApp::new().await;
        factories::user(&app.pool, UserOptions::default()).await;

        let resp = app.get("/login").await;
        resp.assert_status_ok();

        let body_str = resp.text();
        assert!(body_str.contains("Sign in"), "missing title: {body_str}");
        assert!(
            body_str.contains(r#"type="email""#),
            "missing email input: {body_str}"
        );
        assert!(
            body_str.contains(r#"type="password""#),
            "missing password input: {body_str}"
        );
    }

    #[tokio::test]
    async fn login_with_valid_credentials_succeeds() {
        let app = TestApp::new().await;
        let gen_user = factories::user(&app.pool, UserOptions::default()).await;

        let body = form_body(&[
            ("email", &gen_user.user.email),
            ("password", &gen_user.password),
        ]);

        let resp = app.post("/login", &body).await;

        let cookie = resp.header("set-cookie").to_str().unwrap().to_string();

        assert!(cookie.contains("id="), "missing session cookie: {cookie}");
        assert!(cookie.contains("HttpOnly"), "missing HttpOnly: {cookie}");
        assert!(
            cookie.contains("SameSite=Lax"),
            "missing SameSite: {cookie}"
        );
    }

    #[tokio::test]
    async fn login_with_invalid_password_shows_error() {
        let app = TestApp::new().await;
        let gen_user = factories::user(&app.pool, UserOptions::default()).await;

        let body = form_body(&[
            ("email", &gen_user.user.email),
            ("password", "wrong-password"),
        ]);

        let resp = app.post("/login", &body).await;

        resp.assert_status_ok();

        let body_str = resp.text();
        assert!(
            body_str.contains("Invalid email or password"),
            "missing error: {body_str}"
        );
    }

    #[tokio::test]
    async fn login_with_nonexistent_email_shows_error() {
        let app = TestApp::new().await;
        let gen_user = factories::user(&app.pool, UserOptions::default()).await;

        let diff_email: String = SafeEmail().fake();

        let body = form_body(&[("email", &diff_email), ("password", &gen_user.password)]);

        let resp = app.post("/login", &body).await;

        resp.assert_status_ok();

        let body_str = resp.text();
        assert!(
            body_str.contains("Invalid email or password"),
            "missing error: {body_str}"
        );
    }

    #[tokio::test]
    async fn logout_clears_session() {
        let app = TestApp::new().await;
        let gen_user = factories::user(&app.pool, UserOptions::default()).await;

        app.login(&gen_user).await;

        let resp = app.post_htmx("/logout", "").await;

        resp.assert_status_ok();
        assert_eq!(
            resp.header("hx-redirect"),
            "/login",
            "missing HX-Redirect to /login"
        );
    }

    #[tokio::test]
    async fn logout_without_htmx_redirects_to_next() {
        let app = TestApp::new().await;
        let gen_user = factories::user(&app.pool, UserOptions::default()).await;

        app.login(&gen_user).await;

        let next = "/invite?token=7.abc";
        let resp = app.post("/logout", &form_body(&[("next", next)])).await;

        resp.assert_status(StatusCode::SEE_OTHER);
        assert_eq!(resp.header("location"), "/invite?token=7.abc");
        assert!(
            resp.maybe_header("hx-redirect").is_none(),
            "non-htmx request must not get an HX-Redirect header"
        );
    }

    #[tokio::test]
    async fn protected_page_redirects_to_login_when_unauthenticated() {
        let app = TestApp::new().await;
        factories::user(&app.pool, UserOptions::default()).await;

        let resp = app.get("/").await;

        assert!(
            resp.status_code().is_redirection(),
            "expected redirect, got {}",
            resp.status_code()
        );
    }

    /// Login matches email case-insensitively.
    #[tokio::test]
    async fn login_email_is_case_insensitive() {
        let app = TestApp::new().await;
        let gen_user = factories::user(&app.pool, UserOptions::default()).await;

        let body = form_body(&[
            ("email", &gen_user.user.email.to_uppercase()),
            ("password", &gen_user.password),
        ]);

        let resp = app.post("/login", &body).await;

        let cookie = resp.header("set-cookie").to_str().unwrap().to_string();

        assert!(
            cookie.contains("id="),
            "case-variant login must succeed: {cookie}"
        );
    }

    /// htmx: invalid login swaps just the form card fragment (not the full page).
    #[tokio::test]
    async fn login_htmx_invalid_swaps_form_fragment() {
        let app = TestApp::new().await;
        let gen_user = factories::user(&app.pool, UserOptions::default()).await;

        let body = form_body(&[("email", &gen_user.user.email), ("password", "wrong-pass")]);

        let resp = app.post_htmx("/login", &body).await;

        resp.assert_status_ok();
        let body = resp.text();
        assert!(
            body.contains("Invalid email or password"),
            "fragment should carry the error: {body}"
        );
        assert!(
            body.contains(r#"id="login-card""#),
            "fragment should be the login card: {body}"
        );
        assert!(
            !body.contains("<html"),
            "htmx error must be a fragment, not a full page: {body}"
        );
    }

    /// htmx: successful login answers with HX-Redirect for full-page nav.
    #[tokio::test]
    async fn login_htmx_success_returns_hx_redirect() {
        let app = TestApp::new().await;
        let gen_user = factories::user(&app.pool, UserOptions::default()).await;

        let body = form_body(&[
            ("email", &gen_user.user.email),
            ("password", &gen_user.password),
        ]);

        let resp = app.post_htmx("/login", &body).await;

        resp.assert_status_ok();
        assert_eq!(
            resp.header("hx-redirect"),
            "/",
            "successful htmx login should HX-Redirect home"
        );
    }

    #[test]
    fn login_page_renders_with_and_without_error() {
        LoginTemplate::default().render().expect("login renders");
        LoginTemplate {
            error: Some("Invalid email or password".to_string()),
            next: Some("/dashboard".to_string()),
        }
        .render()
        .expect("login renders with error and next");
    }

    #[test]
    fn base_layout_has_no_menu() {
        let html = LoginTemplate::default().render().expect("renders");
        assert!(
            !html.contains("ph-list"),
            "login must not show the hamburger: {html}"
        );
        assert!(
            !html.contains(r#"hx-post="/logout""#),
            "login must not show sign out: {html}"
        );
    }

    #[tokio::test]
    async fn test_login_route_authenticates_by_user_id() {
        let app = TestApp::new().await;
        let gen_user = factories::user(&app.pool, UserOptions::default()).await;

        let resp = app
            .server
            .post(&format!("/__test/login/{}", gen_user.user.id))
            .await;

        resp.assert_status(StatusCode::NO_CONTENT);
        let cookie = resp.header("set-cookie").to_str().unwrap().to_string();
        assert!(cookie.contains("id="), "missing session cookie: {cookie}");
    }
}
