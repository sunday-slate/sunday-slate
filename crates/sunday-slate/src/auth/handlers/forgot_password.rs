use crate::auth::reset;
use crate::web::{FormErrors, FormInput, Valid, redirect};
use crate::{AppError, AppState, form_view};
use askama::Template;
use axum::extract::State;
use axum::response::{Html, IntoResponse};
use axum_htmx::HxRequest;
use axum_messages::Messages;
use garde::Validate;
use serde::Deserialize;

#[derive(Template, Default)]
#[template(path = "auth/forgot_password.html", blocks = ["form"])]
pub struct ForgotPasswordTemplate {
    errors: FormErrors,
    email: String,
}
form_view!(ForgotPasswordTemplate);

#[derive(Deserialize, Validate)]
pub struct ForgotPasswordInput {
    #[garde(email)]
    email: String,
}

impl FormInput for ForgotPasswordInput {
    type View = ForgotPasswordTemplate;
    type Ctx = ();

    fn to_view(&self, (): (), errors: FormErrors) -> Self::View {
        ForgotPasswordTemplate {
            errors,
            email: self.email.clone(),
        }
    }
}

pub async fn forgot_password() -> Result<impl IntoResponse, AppError> {
    Ok(Html(ForgotPasswordTemplate::default().render()?))
}

pub async fn handle_forgot_password(
    HxRequest(is_htmx): HxRequest,
    messages: Messages,
    State(state): State<AppState>,
    Valid(form): Valid<ForgotPasswordInput>,
) -> Result<impl IntoResponse, AppError> {
    tokio::spawn(async move {
        if let Err(err) = reset::send_reset_email(
            &state.db,
            &state.mailer,
            &state.config.base_url,
            &form.email,
        )
        .await
        {
            tracing::error!(error = %err, "password reset task failed");
        }
    });

    let confirmation = "Check your email — if an account exists for that address, \
         we've sent a password reset link. It expires in one hour.";

    messages.success(confirmation);
    Ok(redirect(is_htmx, "/forgot-password"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::factories::UserOptions;
    use crate::tests::utils::*;
    use crate::tests::{TestApp, factories};
    use fake::Fake;
    use fake::faker::internet::en::SafeEmail;

    /// POST /forgot-password returns identical redirects for known/unknown addresses
    /// — the response reveals nothing about account existence.
    #[tokio::test]
    async fn forgot_password_returns_same_redirect_for_any_email() {
        let app = TestApp::new().await;
        let gen_user = factories::user(&app.pool, UserOptions::default()).await;
        let unknown_email: String = SafeEmail().fake();

        for body in [
            form_body(&[("email", &gen_user.user.email)]),
            form_body(&[("email", &unknown_email)]),
        ] {
            let resp = app.post("/forgot-password", &body).await;

            assert!(
                resp.status_code().is_redirection(),
                "expected redirect, got {}",
                resp.status_code()
            );
            assert_eq!(resp.header("location"), "/forgot-password");
        }
    }

    /// POST /forgot-password with an invalid email shows a per-field error
    /// on the form instead of redirecting.
    #[tokio::test]
    async fn forgot_post_with_invalid_email_shows_field_error() {
        let app = TestApp::new().await;
        factories::user(&app.pool, UserOptions::default()).await;

        let resp = app
            .post("/forgot-password", &form_body(&[("email", "not-an-email")]))
            .await;

        resp.assert_status_ok();
        let body = resp.text();
        assert!(
            body.contains("not a valid email"),
            "should show email validation error: {body}"
        );
        assert!(
            body.contains(r#"action="/forgot-password""#),
            "form should still be present: {body}"
        );
        // The confirmation text must NOT appear (we didn't reach the success path).
        assert!(
            !body.contains("Check your email"),
            "should not show confirmation on validation failure: {body}"
        );
    }

    /// Over htmx, POST /forgot-password issues a client-side redirect (HX-Redirect)
    /// to the same page, so the confirmation toast shows on the destination —
    /// identical to the full-page flow.
    #[tokio::test]
    async fn forgot_password_htmx_redirects() {
        let app = TestApp::new().await;
        factories::user(&app.pool, UserOptions::default()).await;

        let email: String = SafeEmail().fake();

        let resp = app
            .post_htmx("/forgot-password", &form_body(&[("email", &email)]))
            .await;

        resp.assert_status_ok();
        assert_eq!(resp.header("hx-redirect"), "/forgot-password");
    }

    #[test]
    fn forgot_password_renders_without_errors() {
        ForgotPasswordTemplate::default()
            .render()
            .expect("forgot renders");
    }

    #[test]
    fn forgot_password_form_fragment_renders() {
        let form = ForgotPasswordInput {
            email: "not-an-email".to_string(),
        };
        let report = form.validate().unwrap_err();
        let template = ForgotPasswordTemplate {
            errors: FormErrors::from(&report),
            email: "bob@example.com".to_string(),
        };
        let html = template.as_form().render().expect("fragment renders");
        assert!(html.contains("not a valid email"));
    }

    #[test]
    fn forgot_form_rejects_invalid_email() {
        let form = ForgotPasswordInput {
            email: "not-an-email".to_string(),
        };
        assert!(form.validate().is_err());
    }

    #[test]
    fn forgot_form_accepts_valid_email() {
        let form = ForgotPasswordInput {
            email: "alice@example.com".to_string(),
        };
        assert!(form.validate().is_ok());
    }
}
