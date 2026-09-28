use crate::auth::Commissioner;
use crate::chrome::Chrome;
use crate::invites::model::InviteLink;
use crate::invites::{service, store};
use crate::leagues::CurrentLeague;
use crate::web::{FormErrors, FormInput, Valid, form_response, redirect};
use crate::{AppError, AppState, form_view};
use askama::Template;
use axum::extract::State;
use axum::response::{Html, IntoResponse};
use axum_htmx::HxRequest;
use axum_messages::Messages;
use garde::Validate;
use serde::Deserialize;
use tower_sessions::Session;

#[derive(Template)]
#[template(path = "invites/new.html", blocks = ["form"])]
pub struct NewInviteTemplate {
    errors: FormErrors,
    email: String,
    chrome: Chrome,
}
form_view!(NewInviteTemplate);

impl Default for NewInviteTemplate {
    fn default() -> Self {
        Self {
            errors: FormErrors::default(),
            email: String::new(),
            chrome: Chrome::focused("Invite a member", "/invites"),
        }
    }
}

#[derive(Deserialize, Validate)]
pub struct InviteInput {
    #[garde(email)]
    email: String,
}

impl FormInput for InviteInput {
    type View = NewInviteTemplate;
    type Ctx = ();

    fn normalize(&mut self) {
        self.email = self.email.trim().to_lowercase();
    }

    fn to_view(&self, (): (), errors: FormErrors) -> Self::View {
        NewInviteTemplate {
            errors,
            email: self.email.clone(),
            ..Default::default()
        }
    }
}

pub async fn new(_: Commissioner) -> Result<impl IntoResponse, AppError> {
    Ok(Html(NewInviteTemplate::default().render()?))
}

pub async fn handle_create(
    _: Commissioner,
    CurrentLeague(league): CurrentLeague,
    HxRequest(is_htmx): HxRequest,
    messages: Messages,
    session: Session,
    State(state): State<AppState>,
    Valid(form): Valid<InviteInput>,
) -> Result<impl IntoResponse, AppError> {
    if store::is_member(state.db.reader(), league.id, &form.email).await? {
        let view = NewInviteTemplate {
            errors: FormErrors::single("email", "That person is already in your league."),
            email: form.email.clone(),
            ..Default::default()
        };
        return form_response(is_htmx, &view);
    }

    let sent = service::send_invite(
        &state.db,
        &state.mailer,
        &state.config.base_url,
        &league,
        &form.email,
    )
    .await?;

    // Carry the link across the redirect: it is the same one the email holds,
    // and it can never be recovered from the database afterwards.
    crate::invites::set_fresh_link(&session, sent.id, InviteLink::Issued(sent.link)).await?;

    messages.success("Invite sent.");
    Ok(redirect(is_htmx, "/invites"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::factories::{LeagueOptions, TeamOptions};
    use crate::tests::utils::*;
    use crate::tests::{TestApp, factories};
    use fake::Fake;
    use fake::faker::internet::en::SafeEmail;
    use http::StatusCode;

    #[test]
    fn templates_render() {
        NewInviteTemplate::default().render().expect("new page");
        NewInviteTemplate {
            errors: FormErrors::single("email", "That person is already in your league."),
            email: "x@y.com".to_string(),
            ..Default::default()
        }
        .as_form()
        .render()
        .expect("new fragment with error");
    }

    /// POST /invites with the commissioner's own email re-renders the form
    /// with an "already in your league" error and creates no invite.
    #[tokio::test]
    async fn inviting_existing_member_is_rejected() {
        let app = TestApp::new().await;
        let gen_league = factories::league(&app.pool, LeagueOptions::default()).await;

        app.login_as(&gen_league.commish).await;

        let gen_team = factories::team(
            &app.pool,
            TeamOptions {
                league: Some(gen_league.league.clone()),
                ..Default::default()
            },
        )
        .await;

        let resp = app
            .post("/invites", &form_body(&[("email", &gen_team.owner.email)]))
            .await;

        resp.assert_status_ok();
        let body = resp.text();
        assert!(
            body.contains("already in your league"),
            "error copy: {body}"
        );

        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM invites")
            .fetch_one(&app.pool)
            .await
            .expect("count");
        assert_eq!(count, 0);
    }

    /// POST /invites with valid input creates an invite row and redirects.
    #[tokio::test]
    async fn creates_invite_and_redirects() {
        let app = TestApp::new().await;
        let gen_league = factories::league(&app.pool, LeagueOptions::default()).await;

        app.login_as(&gen_league.commish).await;

        let email: String = SafeEmail().fake();

        let resp = app.post("/invites", &form_body(&[("email", &email)])).await;

        resp.assert_status(StatusCode::SEE_OTHER);
        assert_eq!(resp.header("location"), "/invites");
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM invites")
            .fetch_one(&app.pool)
            .await
            .expect("count");
        assert_eq!(count, 1);
    }

    /// Creating an invite shows the accept link straight away, and it is the
    /// SAME link the email carries — asking for it must not retire the one the
    /// invitee was just sent.
    #[tokio::test]
    async fn creating_an_invite_shows_the_link_that_was_emailed() {
        let app = TestApp::new().await;
        let gen_league = factories::league(&app.pool, LeagueOptions::default()).await;

        app.login_as(&gen_league.commish).await;

        let email: String = SafeEmail().fake();
        let resp = app.post("/invites", &form_body(&[("email", &email)])).await;
        resp.assert_status(StatusCode::SEE_OTHER);
        assert_eq!(resp.header("location"), "/invites");

        let shown = invite_token_in(&app.get("/invites").await.text());

        let sent = app.mailer.all();
        assert_eq!(sent.len(), 1, "one invite email");
        assert_eq!(
            shown,
            invite_token_in(&sent[0].html),
            "the link on screen must be the emailed link, not a replacement"
        );

        assert!(
            crate::invites::store::verify(&app.state.db, &shown)
                .await
                .expect("verify")
                .is_some(),
            "the emailed link must still work"
        );
    }

    /// The link is shown once. A later visit to the list must not still be
    /// handing it out — it lives in the session for exactly one render.
    #[tokio::test]
    async fn the_shown_link_is_cleared_after_one_view() {
        let app = TestApp::new().await;
        let gen_league = factories::league(&app.pool, LeagueOptions::default()).await;

        app.login_as(&gen_league.commish).await;

        let email: String = SafeEmail().fake();
        app.post("/invites", &form_body(&[("email", &email)])).await;

        let first = app.get("/invites").await.text();
        assert!(
            first.contains("/invite?token="),
            "first view shows the link"
        );

        let second = app.get("/invites").await.text();
        assert!(
            !second.contains("/invite?token="),
            "the link must not persist on the list: {second}"
        );
    }

    /// POST /invites with an htmx request and an existing-member email
    /// renders only the form fragment
    #[tokio::test]
    async fn htmx_rejection_renders_form_fragment() {
        let app = TestApp::new().await;
        let gen_league = factories::league(&app.pool, LeagueOptions::default()).await;

        app.login_as(&gen_league.commish).await;

        let gen_team = factories::team(
            &app.pool,
            TeamOptions {
                league: Some(gen_league.league.clone()),
                ..Default::default()
            },
        )
        .await;

        let resp = app
            .post_htmx("/invites", &form_body(&[("email", &gen_team.owner.email)]))
            .await;

        resp.assert_status_ok();
        let body = resp.text();
        assert!(
            body.contains("already in your league"),
            "error copy in fragment: {body}"
        );
        assert!(
            !body.contains(r#"id="menu-toggle""#),
            "fragment must not contain full-page chrome: {body}"
        );
    }
}
