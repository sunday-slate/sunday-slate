use crate::auth::password;
use crate::auth::password::Password;
use crate::auth::reset;
use crate::auth::store;
use crate::users::store as users;
use crate::web::{FormErrors, FormInput, Valid, redirect};
use crate::{AppError, AppState, form_view};
use askama::Template;
use axum::extract::{Query, State};
use axum::response::{Html, IntoResponse, Redirect};
use axum_htmx::HxRequest;
use axum_messages::Messages;
use garde::Validate;
use serde::Deserialize;

#[derive(Template, Default)]
#[template(path = "auth/reset_password.html", blocks = ["form"])]
pub struct ResetPasswordTemplate {
    token: String,
    errors: FormErrors,
}
form_view!(ResetPasswordTemplate);

#[derive(Deserialize)]
pub struct ResetPasswordQuery {
    token: Option<String>,
}

#[derive(Deserialize, Validate)]
pub struct ResetPasswordInput {
    #[garde(skip)]
    token: String,
    #[garde(dive)]
    password: Password,
    #[garde(matches(password))]
    password_confirm: Password,
}

impl FormInput for ResetPasswordInput {
    type View = ResetPasswordTemplate;
    type Ctx = ();

    fn to_view(&self, (): (), errors: FormErrors) -> Self::View {
        ResetPasswordTemplate {
            token: self.token.clone(),
            errors,
        }
    }
}

pub async fn reset_password(
    State(state): State<AppState>,
    messages: Messages,
    Query(params): Query<ResetPasswordQuery>,
) -> Result<impl IntoResponse, AppError> {
    let valid = match &params.token {
        Some(raw) => reset::verify(&state.db, raw).await?.is_some(),
        None => false,
    };

    if !valid {
        messages.error(
            "The password reset link is invalid or has expired. Request a new one to continue.",
        );
        return Ok(Redirect::to("/forgot-password").into_response());
    }

    let template = ResetPasswordTemplate {
        token: params.token.unwrap_or_default(),
        errors: FormErrors::default(),
    };

    Ok(Html(template.render()?).into_response())
}

pub async fn handle_reset_password(
    HxRequest(is_htmx): HxRequest,
    messages: Messages,
    State(state): State<AppState>,
    Valid(form): Valid<ResetPasswordInput>,
) -> Result<impl IntoResponse, AppError> {
    let Some(verified) = reset::verify(&state.db, &form.token).await? else {
        messages.error(
            "The password reset link is invalid or has expired. Request a new one to continue.",
        );
        return Ok(redirect(is_htmx, "/forgot-password"));
    };

    let new_hash = password::hash(&form.password.0);
    state
        .db
        .write_tx(async |conn| -> Result<(), crate::AppError> {
            users::update_password_hash(&mut *conn, verified.user_id, &new_hash).await?;
            store::mark_token_used(&mut *conn, verified.token_id).await?;
            Ok(())
        })
        .await?;

    messages.success("Your password has been reset. Sign in with your new password.");
    Ok(redirect(is_htmx, "/login"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::password::Password;
    use crate::tests::factories::{ResetTokenOptions, UserOptions};
    use crate::tests::utils::*;
    use crate::tests::{TestApp, factories};

    /// A valid token renders the set-password form.
    #[tokio::test]
    async fn reset_get_valid_token_shows_form() {
        let app = TestApp::new().await;
        let gen_reset = factories::reset_token(&app.pool, ResetTokenOptions::default()).await;
        let token = gen_reset.token();

        let resp = app.get(&format!("/reset-password?token={token}")).await;

        resp.assert_status_ok();
        let body = resp.text();
        assert!(body.contains("Set a new password"), "missing form: {body}");
        assert!(
            body.contains(r#"name="password_confirm""#),
            "missing confirm field: {body}"
        );
        assert!(
            body.contains(r#"<a href="/login" class="link link-hover">Back to sign in</a>"#),
            "missing back link: {body}"
        );
    }

    /// A malformed/unknown token redirects to /forgot-password and carries an
    /// explanatory flash that surfaces on the destination page.
    #[tokio::test]
    async fn reset_get_invalid_token_redirects_with_flash() {
        let app = TestApp::new().await;
        factories::user(&app.pool, UserOptions::default()).await;

        let resp = app.get("/reset-password?token=999.bogus").await;

        assert!(resp.status_code().is_redirection());
        assert_eq!(resp.header("location"), "/forgot-password");

        // Following the redirect shows the one-shot error flash (the jar carries
        // the flash cookie from the redirect response automatically).
        let followed = app.get("/forgot-password").await;
        followed.assert_status_ok();
        let body = followed.text();
        assert!(
            body.contains("link is invalid or has expired"),
            "missing invalid-link flash: {body}"
        );
    }

    /// An expired token redirects to /forgot-password.
    #[tokio::test]
    async fn reset_get_expired_token_redirects() {
        let app = TestApp::new().await;
        let gen_reset =
            factories::expired_reset_token(&app.pool, ResetTokenOptions::default()).await;
        let token = gen_reset.token();

        let resp = app.get(&format!("/reset-password?token={token}")).await;

        assert!(resp.status_code().is_redirection());
        assert_eq!(resp.header("location"), "/forgot-password");
    }

    /// Mismatched passwords re-render the form with an error and do not change the
    /// stored password.
    #[tokio::test]
    async fn reset_post_mismatch_keeps_form() {
        let app = TestApp::new().await;
        let gen_user = factories::user(&app.pool, UserOptions::default()).await;
        let gen_reset = factories::reset_token(
            &app.pool,
            ResetTokenOptions {
                user: Some(gen_user.user.clone()),
            },
        )
        .await;
        let token = gen_reset.token();

        let resp = app
            .post(
                "/reset-password",
                &form_body(&[
                    ("token", &token),
                    ("password", "newpass123"),
                    ("password_confirm", "different"),
                ]),
            )
            .await;

        resp.assert_status_ok();
        let body = resp.text();
        assert!(
            body.contains("does not match password field"),
            "missing error: {body}"
        );

        // Original password still works.
        let cookie = app.login(&gen_user).await;
        assert!(
            cookie.contains("id="),
            "original password must be unchanged"
        );
    }

    /// A new password shorter than the 8-character minimum re-renders the form with
    /// an error and leaves the stored password unchanged.
    #[tokio::test]
    async fn reset_post_short_password_keeps_form() {
        let app = TestApp::new().await;
        let gen_user = factories::user(&app.pool, UserOptions::default()).await;
        let gen_reset = factories::reset_token(
            &app.pool,
            ResetTokenOptions {
                user: Some(gen_user.user.clone()),
            },
        )
        .await;
        let token = gen_reset.token();

        let resp = app
            .post(
                "/reset-password",
                &form_body(&[
                    ("token", &token),
                    ("password", "short"),
                    ("password_confirm", "short"),
                ]),
            )
            .await;

        resp.assert_status_ok();
        let body = resp.text();
        assert!(
            body.contains("length is lower than 8"),
            "missing length error: {body}"
        );

        // Original password still works (no change applied).
        let cookie = app.login(&gen_user).await;
        assert!(
            cookie.contains("id="),
            "original password must be unchanged"
        );
    }

    /// An invalid token on POST redirects to /forgot-password with an explanatory
    /// flash, rather than rendering an invalid-link page.
    #[tokio::test]
    async fn reset_post_invalid_token_redirects_with_flash() {
        let app = TestApp::new().await;
        factories::user(&app.pool, UserOptions::default()).await;

        let resp = app
            .post(
                "/reset-password",
                &form_body(&[
                    ("token", "999.bogus"),
                    ("password", "newpass123"),
                    ("password_confirm", "newpass123"),
                ]),
            )
            .await;

        assert!(resp.status_code().is_redirection());
        assert_eq!(resp.header("location"), "/forgot-password");

        // Following the redirect shows the one-shot error flash (the jar carries
        // the flash cookie from the redirect response automatically).
        let followed = app.get("/forgot-password").await;
        followed.assert_status_ok();
        let body = followed.text();
        assert!(
            body.contains("link is invalid or has expired"),
            "missing invalid-link flash: {body}"
        );
    }

    /// htmx: successful reset answers with HX-Redirect to /login.
    #[tokio::test]
    async fn reset_password_htmx_success_returns_hx_redirect() {
        let app = TestApp::new().await;
        let gen_reset = factories::reset_token(&app.pool, ResetTokenOptions::default()).await;
        let token = gen_reset.token();

        let resp = app
            .post_htmx(
                "/reset-password",
                &form_body(&[
                    ("token", &token),
                    ("password", "newpass123"),
                    ("password_confirm", "newpass123"),
                ]),
            )
            .await;

        resp.assert_status_ok();
        assert_eq!(
            resp.header("hx-redirect"),
            "/login",
            "successful htmx reset should HX-Redirect to login"
        );
    }

    /// htmx: password mismatch swaps just the form fragment (not the full page).
    #[tokio::test]
    async fn reset_password_htmx_mismatch_swaps_form_fragment() {
        let app = TestApp::new().await;
        let gen_reset = factories::reset_token(&app.pool, ResetTokenOptions::default()).await;
        let token = gen_reset.token();

        let resp = app
            .post_htmx(
                "/reset-password",
                &form_body(&[
                    ("token", &token),
                    ("password", "newpass123"),
                    ("password_confirm", "different"),
                ]),
            )
            .await;

        resp.assert_status_ok();
        let body = resp.text();
        assert!(
            body.contains("does not match password field"),
            "fragment should carry the error: {body}"
        );
        assert!(
            !body.contains("<html"),
            "htmx error must be a fragment, not a full page: {body}"
        );
    }

    /// A short password that also doesn't match its confirm produces all applicable
    /// errors on both fields: length errors on both + mismatch on confirm.
    #[tokio::test]
    async fn reset_post_with_invalid_password_and_mismatch_reports_both() {
        let app = TestApp::new().await;
        let gen_reset = factories::reset_token(&app.pool, ResetTokenOptions::default()).await;
        let token = gen_reset.token();

        let resp = app
            .post(
                "/reset-password",
                &form_body(&[
                    ("token", &token),
                    ("password", "short"),
                    ("password_confirm", "differ"),
                ]),
            )
            .await;

        resp.assert_status_ok();
        let body = resp.text();
        assert!(
            body.contains("length is lower than 8"),
            "password should report length error: {body}"
        );
        assert!(
            body.contains("does not match password field"),
            "password_confirm should report mismatch error: {body}"
        );
    }

    #[test]
    fn reset_password_renders_valid_and_invalid() {
        ResetPasswordTemplate {
            token: "42.abc".into(),
            errors: FormErrors::default(),
        }
        .render()
        .expect("valid");
        let bad_form = ResetPasswordInput {
            token: "42.abc".to_string(),
            password: Password("hunter22".to_string()),
            password_confirm: Password("hunter23".to_string()),
        };
        let report = bad_form.validate().unwrap_err();

        ResetPasswordTemplate {
            token: "42.abc".into(),
            errors: FormErrors::from(&report),
        }
        .render()
        .expect("valid with error");
        ResetPasswordTemplate::default().render().expect("invalid");
    }

    #[test]
    fn reset_form_rejects_mismatched_passwords() {
        let form = ResetPasswordInput {
            token: "42.abc".to_string(),
            password: Password("hunter22".to_string()),
            password_confirm: Password("hunter23".to_string()),
        };
        let Err(report) = form.validate() else {
            panic!("expected error");
        };
        let errors = FormErrors::from(&report);
        let msgs = errors.field("password_confirm");
        assert!(
            msgs.iter()
                .any(|m| m.contains("does not match password field"))
        );
    }

    #[test]
    fn reset_form_rejects_short_password() {
        let form = ResetPasswordInput {
            token: "42.abc".to_string(),
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
    fn reset_form_accepts_valid_form() {
        let form = ResetPasswordInput {
            token: "42.abc".to_string(),
            password: Password("hunter22".to_string()),
            password_confirm: Password("hunter22".to_string()),
        };
        assert!(form.validate().is_ok());
    }

    #[test]
    fn reset_form_reports_multiple_errors() {
        let form = ResetPasswordInput {
            token: "42.abc".to_string(),
            password: Password("sh ort".to_string()),
            password_confirm: Password("di ff".to_string()),
        };
        let Err(report) = form.validate() else {
            panic!("expected error");
        };
        let errors = FormErrors::from(&report);
        assert!(errors.field("password").len() >= 2);
        assert_eq!(
            errors.field("password_confirm"),
            vec!["does not match password field"]
        );
    }
}
