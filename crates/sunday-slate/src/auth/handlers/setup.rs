use crate::auth::password;
use crate::auth::password::Password;
use crate::users::store as users;
use crate::web::{FormErrors, form_response, redirect};
use crate::{AppError, AppState, Db, User, form_view};
use askama::Template;
use axum::Form;
use axum::extract::State;
use axum::response::{Html, IntoResponse, Redirect};
use axum_htmx::HxRequest;
use axum_login::AuthSession;
use garde::Validate;
use serde::Deserialize;

#[derive(Template, Default)]
#[template(path = "auth/setup.html", blocks = ["form"])]
struct SetupTemplate {
    errors: FormErrors,
    email: String,
}
form_view!(SetupTemplate);

#[derive(Deserialize, Validate)]
pub struct SetupInput {
    #[garde(email)]
    email: String,
    #[garde(dive)]
    password: Password,
    #[garde(matches(password))]
    password_confirm: Password,
}

pub async fn setup(State(state): State<AppState>) -> Result<impl IntoResponse, AppError> {
    if users::exists(state.db.reader()).await? {
        return Ok(Redirect::to("/login").into_response());
    }
    Ok(Html(SetupTemplate::default().render()?).into_response())
}

pub async fn handle_setup(
    HxRequest(is_htmx): HxRequest,
    auth_session: AuthSession<Db>,
    State(state): State<AppState>,
    Form(form): Form<SetupInput>,
) -> Result<impl IntoResponse, AppError> {
    if users::exists(state.db.reader()).await? {
        return Ok(Redirect::to("/login").into_response());
    }

    if let Err(report) = form.validate() {
        let template = SetupTemplate {
            errors: FormErrors::from(&report),
            email: form.email,
        };
        return form_response(is_htmx, &template);
    }

    let hash = password::hash(&form.password.0);
    let user = state
        .db
        .write_tx(async |conn| -> Result<User, sqlx::Error> {
            users::create(&mut *conn, &form.email, &hash, true).await
        })
        .await?;

    auth_session.login(&user).await?;

    Ok(redirect(is_htmx, "/"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::password::Password;
    use crate::tests::factories::UserOptions;
    use crate::tests::utils::*;
    use crate::tests::{TestApp, factories};
    use http::StatusCode;

    #[tokio::test]
    async fn setup_form_renders_after_redirect() {
        let app = TestApp::new().await;

        let resp = app.get("/setup").await;

        resp.assert_status_ok();
        let body = resp.text();
        assert!(
            body.contains("Welcome to Sunday Slate"),
            "missing title: {body}"
        );
        assert!(
            body.contains(r#"action="/setup""#),
            "missing form action: {body}"
        );
        assert!(
            body.contains(r#"name="email""#),
            "missing email input: {body}"
        );
        assert!(
            body.contains(r#"name="password""#),
            "missing password input: {body}"
        );
        assert!(
            body.contains(r#"name="password_confirm""#),
            "missing password_confirm input: {body}"
        );
    }

    #[tokio::test]
    async fn setup_post_with_short_password_shows_error() {
        let app = TestApp::new().await;

        let resp = app
            .post(
                "/setup",
                &form_body(&[
                    ("email", "alice@example.com"),
                    ("password", "short"),
                    ("password_confirm", "short"),
                ]),
            )
            .await;

        resp.assert_status_ok();
        let body = resp.text();
        assert!(
            body.contains("length is lower than 8"),
            "missing error: {body}"
        );
        assert!(
            body.contains("alice@example.com"),
            "email should be pre-filled: {body}"
        );
        assert!(
            !body.contains(r#"value="short""#),
            "password must not be echoed: {body}"
        );
    }

    #[tokio::test]
    async fn setup_post_with_mismatched_passwords_shows_error() {
        let app = TestApp::new().await;

        let resp = app
            .post(
                "/setup",
                &form_body(&[
                    ("email", "alice@example.com"),
                    ("password", "hunter22"),
                    ("password_confirm", "hunter23"),
                ]),
            )
            .await;

        resp.assert_status_ok();
        let body = resp.text();
        assert!(
            body.contains("does not match password field"),
            "missing error: {body}"
        );
    }

    #[tokio::test]
    async fn setup_post_with_empty_email_shows_field_error() {
        let app = TestApp::new().await;

        let resp = app
            .post(
                "/setup",
                &form_body(&[
                    ("email", ""),
                    ("password", "hunter22"),
                    ("password_confirm", "hunter22"),
                ]),
            )
            .await;

        resp.assert_status_ok();
        let body = resp.text();
        assert!(body.contains("not a valid email"), "missing error: {body}");
    }

    #[tokio::test]
    async fn setup_post_with_valid_creates_admin_and_logs_in() {
        let app = TestApp::new().await;

        let resp = app
            .post(
                "/setup",
                &form_body(&[
                    ("email", "alice@example.com"),
                    ("password", "hunter22"),
                    ("password_confirm", "hunter22"),
                ]),
            )
            .await;

        assert!(
            resp.status_code().is_redirection(),
            "expected redirect, got {}",
            resp.status_code()
        );
        assert_eq!(resp.header("location"), "/");

        assert!(
            resp.maybe_cookie("id").is_some(),
            "missing session cookie after setup"
        );

        let user = sqlx::query_as::<_, crate::User>(
            "SELECT id, email, password_hash, is_admin, created_at, updated_at \
             FROM users WHERE email = ?",
        )
        .bind("alice@example.com")
        .fetch_one(&app.pool)
        .await
        .expect("find created user");
        assert!(user.is_admin, "first user must be admin");

        let home = app.get("/").await;
        home.assert_status(StatusCode::SEE_OTHER);
        assert_eq!(
            home.header("location"),
            "/leagues/new",
            "first user still needs to create a league"
        );
    }

    #[tokio::test]
    async fn setup_get_after_first_user_redirects_to_login() {
        let app = TestApp::new().await;
        factories::user(&app.pool, UserOptions::default()).await;
        let resp = app.get("/setup").await;

        assert!(
            resp.status_code().is_redirection(),
            "expected redirect, got {}",
            resp.status_code()
        );
        assert_eq!(resp.header("location"), "/login");
    }

    #[tokio::test]
    async fn setup_post_after_first_user_redirects_to_login() {
        let app = TestApp::new().await;
        factories::user(&app.pool, UserOptions::default()).await;
        let resp = app
            .post(
                "/setup",
                &form_body(&[
                    ("email", "bob@example.com"),
                    ("password", "hunter22"),
                    ("password_confirm", "hunter22"),
                ]),
            )
            .await;

        assert!(
            resp.status_code().is_redirection(),
            "expected redirect, got {}",
            resp.status_code()
        );
        assert_eq!(resp.header("location"), "/login");

        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
            .fetch_one(&app.pool)
            .await
            .expect("count users");
        assert_eq!(count, 1, "setup POST must not create a second user");
    }

    #[tokio::test]
    async fn setup_htmx_invalid_swaps_form_fragment() {
        let app = TestApp::new().await;

        let resp = app
            .post_htmx(
                "/setup",
                &form_body(&[
                    ("email", "alice@example.com"),
                    ("password", "short"),
                    ("password_confirm", "short"),
                ]),
            )
            .await;

        resp.assert_status_ok();
        let body = resp.text();
        assert!(
            body.contains("length is lower than 8"),
            "fragment should carry the error: {body}"
        );
        assert!(
            !body.contains("<html"),
            "htmx error must be a fragment, not a full page: {body}"
        );
    }

    #[tokio::test]
    async fn setup_htmx_valid_returns_hx_redirect() {
        let app = TestApp::new().await;

        let resp = app
            .post_htmx(
                "/setup",
                &form_body(&[
                    ("email", "alice@example.com"),
                    ("password", "hunter22"),
                    ("password_confirm", "hunter22"),
                ]),
            )
            .await;

        resp.assert_status_ok();
        assert_eq!(
            resp.header("hx-redirect"),
            "/",
            "successful htmx setup should HX-Redirect home"
        );
    }

    #[tokio::test]
    async fn setup_post_with_invalid_email_and_short_password_reports_both() {
        let app = TestApp::new().await;

        let resp = app
            .post(
                "/setup",
                &form_body(&[
                    ("email", "not-an-email"),
                    ("password", "short"),
                    ("password_confirm", "short"),
                ]),
            )
            .await;

        resp.assert_status_ok();
        let body = resp.text();
        assert!(
            body.contains("not a valid email"),
            "should report email error: {body}"
        );
        assert!(
            body.contains("length is lower than 8"),
            "should report password length error: {body}"
        );
        assert!(
            !body.contains("alice@example.com"),
            "should not contain unrelated text"
        );
    }

    #[test]
    fn setup_page_renders_with_and_without_errors() {
        SetupTemplate::default()
            .render()
            .expect("setup page renders empty");

        let bad_form = SetupInput {
            email: "not-an-email".to_string(),
            password: Password("hunter22".to_string()),
            password_confirm: Password("hunter22".to_string()),
        };
        let report = bad_form.validate().unwrap_err();
        let errors = FormErrors::from(&report);

        SetupTemplate {
            errors,
            email: "alice@example.com".to_string(),
        }
        .render()
        .expect("setup page renders with errors and prefill");
    }

    #[test]
    fn setup_form_fragment_renders() {
        let bad_form = SetupInput {
            email: "ok@b.com".to_string(),
            password: Password("hunter22".to_string()),
            password_confirm: Password("hunter23".to_string()),
        };
        let report = bad_form.validate().unwrap_err();

        SetupTemplate {
            errors: FormErrors::from(&report),
            email: "bob@example.com".to_string(),
        }
        .as_form()
        .render()
        .expect("fragment renders");
    }

    #[test]
    fn setup_page_does_not_show_menu() {
        let html = SetupTemplate::default().render().expect("renders");
        assert!(
            !html.contains("ph-list"),
            "setup must not show the hamburger: {html}"
        );
        assert!(
            !html.contains(r#"hx-post="/logout""#),
            "setup must not show sign out: {html}"
        );
    }

    #[test]
    fn setup_form_rejects_invalid_email() {
        let form = SetupInput {
            email: "not-an-email".to_string(),
            password: Password("hunter22".to_string()),
            password_confirm: Password("hunter22".to_string()),
        };
        let Err(report) = form.validate() else {
            panic!("expected error");
        };
        let errors = FormErrors::from(&report);
        let msgs = errors.field("email");
        assert!(msgs.iter().any(|m| m.contains("not a valid email")));
    }

    #[test]
    fn setup_form_rejects_short_password() {
        let form = SetupInput {
            email: "a@b.com".to_string(),
            password: Password("short".to_string()),
            password_confirm: Password("short".to_string()),
        };
        let Err(report) = form.validate() else {
            panic!("expected error");
        };
        let errors = FormErrors::from(&report);
        let msgs = errors.field("password");
        assert!(msgs.iter().any(|m| m.contains("length is lower than 8")));
    }

    #[test]
    fn setup_form_rejects_password_with_whitespace() {
        let form = SetupInput {
            email: "a@b.com".to_string(),
            password: Password("short pass".to_string()),
            password_confirm: Password("short pass".to_string()),
        };
        let Err(report) = form.validate() else {
            panic!("expected error");
        };
        let errors = FormErrors::from(&report);
        let msgs = errors.field("password");
        assert!(msgs.iter().any(|m| m.contains("must not contain spaces")));
    }

    #[test]
    fn setup_form_rejects_mismatched_passwords() {
        let form = SetupInput {
            email: "a@b.com".to_string(),
            password: Password("hunter22".to_string()),
            password_confirm: Password("hunter23".to_string()),
        };
        let Err(report) = form.validate() else {
            panic!("expected error");
        };
        let errors = FormErrors::from(&report);
        let msgs = errors.field("password_confirm");
        assert_eq!(msgs, vec!["does not match password field"]);
    }

    #[test]
    fn setup_form_accepts_valid_form() {
        let form = SetupInput {
            email: "a@b.com".to_string(),
            password: Password("hunter22".to_string()),
            password_confirm: Password("hunter22".to_string()),
        };
        assert!(form.validate().is_ok());
    }

    #[test]
    fn setup_form_reports_multiple_field_errors() {
        let form = SetupInput {
            email: "not-an-email".to_string(),
            password: Password("sh ort".to_string()),
            password_confirm: Password("sh ort".to_string()),
        };
        let Err(report) = form.validate() else {
            panic!("expected error");
        };
        let errors = FormErrors::from(&report);
        assert!(!errors.field("email").is_empty());
        assert!(errors.field("password").len() >= 2);
    }
}
