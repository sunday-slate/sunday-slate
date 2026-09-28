use crate::auth::password;
use crate::auth::password::Password;
use crate::invites::extract::NewInvitee;
use crate::web::{FormErrors, form_response, redirect};
use crate::{AppError, AppState, Db, form_view};
use askama::Template;
use axum::Form;
use axum::extract::State;
use axum::response::{Html, IntoResponse, Response};
use axum_htmx::HxRequest;
use axum_login::AuthSession;
use garde::Validate;
use serde::Deserialize;
use tower_sessions::Session;

#[derive(Template, Default)]
#[template(path = "invites/create_user.html", blocks = ["form"])]
pub struct InviteCreateUserTemplate {
    token: String,
    email: String,
    errors: FormErrors,
}
form_view!(InviteCreateUserTemplate);

#[derive(Deserialize, Validate)]
pub struct InviteCreateUserInput {
    #[garde(skip)]
    token: String,
    #[garde(dive)]
    password: Password,
    #[garde(matches(password))]
    password_confirm: Password,
}

/// GET /invite/create-user?token= — render the new-invitee password form.
pub async fn create_user(NewInvitee { invite, token }: NewInvitee) -> Result<Response, AppError> {
    let template = InviteCreateUserTemplate {
        token,
        email: invite.email,
        errors: FormErrors::default(),
    };
    Ok(Html(template.render()?).into_response())
}

/// POST /invite/create-user — create the user, log in, go to team setup.
pub async fn handle_create_user(
    HxRequest(is_htmx): HxRequest,
    auth_session: AuthSession<Db>,
    session: Session,
    State(state): State<AppState>,
    Form(form): Form<InviteCreateUserInput>,
) -> Result<impl IntoResponse, AppError> {
    let token = form.token.as_str();
    let current_user = auth_session.user().await;
    let new_invitee = match NewInvitee::resolve(is_htmx, &state, token, current_user.as_ref()).await
    {
        Ok(v) => v,
        Err(resp) => return Ok(*resp),
    };

    let invite = new_invitee.invite;

    if let Err(report) = form.validate() {
        let template = InviteCreateUserTemplate {
            token: form.token,
            email: invite.email,
            errors: FormErrors::from(&report),
        };
        return form_response(is_htmx, &template);
    }

    let hash = password::hash(&form.password.0);
    let user = state
        .db
        .write_tx(async |conn| -> Result<crate::User, sqlx::Error> {
            crate::users::store::create(&mut *conn, &invite.email, &hash, false).await
        })
        .await?;
    auth_session.login(&user).await?;
    crate::invites::set_pending(&session, invite.invite_id).await?;

    Ok(redirect(is_htmx, "/teams/new"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::factories::{InviteOptions, UserOptions};
    use crate::tests::utils::*;
    use crate::tests::{TestApp, factories};
    use crate::users::store as users;
    use fake::Fake;
    use fake::faker::internet::en::Password as FakePassword;
    use http::StatusCode;
    use rstest::rstest;

    #[test]
    fn create_user_form_renders() {
        InviteCreateUserTemplate {
            token: "7.abc".to_string(),
            email: "a@b.com".to_string(),
            errors: FormErrors::default(),
        }
        .render()
        .expect("page");
    }

    /// GET /invite/create-user?token= when logged in as the wrong user
    /// redirects back to the invite page.
    #[tokio::test]
    async fn create_user_page_redirects_when_logged_in_as_wrong_user() {
        let app = TestApp::new().await;
        let gen_user = factories::user(&app.pool, UserOptions::default()).await;

        app.login(&gen_user).await;

        let gen_invite = factories::invite(&app.pool, InviteOptions::default()).await;
        let token = gen_invite.token();

        let resp = app
            .get(&format!(
                "/invite/create-user?token={}",
                urlencoding::encode(&token)
            ))
            .await;

        resp.assert_status(StatusCode::SEE_OTHER);
        let loc = resp.header("location");
        let loc = loc.to_str().unwrap();
        assert_eq!(
            loc,
            format!("/invite?token={}", urlencoding::encode(&token))
        );
    }

    /// POST /invite/create-user when logged in as the wrong user redirects and
    /// does NOT create an account.
    #[tokio::test]
    async fn create_user_post_rejected_when_logged_in_as_wrong_user() {
        let app = TestApp::new().await;
        let gen_user = factories::user(&app.pool, UserOptions::default()).await;

        app.login(&gen_user).await;

        let gen_invite = factories::invite(&app.pool, InviteOptions::default()).await;
        let token = gen_invite.token();

        let password: String = FakePassword(8..16).fake();
        let body = form_body(&[
            ("token", &token),
            ("password", &password),
            ("password_confirm", &password),
        ]);
        let resp = app.post("/invite/create-user", &body).await;

        resp.assert_status(StatusCode::SEE_OTHER);
        let loc = resp.header("location");
        let loc = loc.to_str().unwrap();
        assert_eq!(
            loc,
            format!("/invite?token={}", urlencoding::encode(&token))
        );

        let u = users::find_by_email(&app.pool, &gen_invite.email)
            .await
            .expect("user query");
        assert!(u.is_none(), "shouldn't create user");

        // /standings 200s for any signed-in member (no team needed, unlike
        // `/`); a logout would redirect to /login.
        let resp = app.get("/standings").await;
        resp.assert_status_ok();
    }

    /// POST /invite/create-user rejects bad passwords (short, mismatched, blank)
    /// with form re-render containing the validation error.
    #[rstest]
    #[case::short_password("ab", "ab", "length")]
    #[case::mismatched("hunter22", "wrong", "does not match password field")]
    #[case::blank_whitespace("    ", "    ", "must not contain spaces")]
    #[tokio::test]
    async fn create_user_rejects_bad_passwords(
        #[case] password: &str,
        #[case] confirm: &str,
        #[case] expected_error_substr: &str,
    ) {
        let app = TestApp::new().await;
        let gen_invite = factories::invite(&app.pool, InviteOptions::default()).await;
        let token = gen_invite.token();

        let body = form_body(&[
            ("token", &token),
            ("password", password),
            ("password_confirm", confirm),
        ]);

        let resp = app.post("/invite/create-user", &body).await;
        resp.assert_status(StatusCode::OK);
        let body_text = resp.text();
        assert!(
            body_text.contains(expected_error_substr),
            "expected error '{expected_error_substr}' not found in: {body_text}"
        );

        let u = users::find_by_email(&app.pool, &gen_invite.email)
            .await
            .expect("user query");
        assert!(u.is_none(), "shouldn't create user");
    }
}
