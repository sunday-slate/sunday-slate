use crate::AppError;
use crate::invites::extract::{Invitation, InviteState};
use askama::Template;
use axum::response::{Html, IntoResponse};

#[derive(Template, Default)]
#[template(path = "invites/show.html")]
pub struct InviteShowTemplate {
    valid: bool,
    league_name: String,
    email: String,
    token: String,
    mismatched_email: Option<String>,
}

/// GET /invite?token= — render the invite's state. Never redirects on a bad token.
pub async fn show(invite: Invitation) -> Result<impl IntoResponse, AppError> {
    let template = match invite.invite_state {
        InviteState::Invalid => InviteShowTemplate::default(),
        InviteState::ExistingUser(v) | InviteState::NewUser(v) => InviteShowTemplate {
            valid: true,
            league_name: v.league_name,
            email: v.email,
            token: invite.token,
            mismatched_email: invite.mismatched_email,
        },
    };
    Ok(Html(template.render()?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::factories::{InviteOptions, LeagueOptions};

    use crate::tests::{TestApp, factories};

    #[test]
    fn templates_render() {
        InviteShowTemplate {
            valid: true,
            league_name: "Sunday Funday".to_string(),
            email: "a@b.com".to_string(),
            token: "7.abc".to_string(),
            mismatched_email: None,
        }
        .render()
        .expect("valid show");

        InviteShowTemplate::default()
            .render()
            .expect("invalid show");

        InviteShowTemplate {
            valid: true,
            league_name: "Sunday Funday".to_string(),
            email: "invited@example.com".to_string(),
            token: "7.abc".to_string(),
            mismatched_email: Some("someone-else@example.com".to_string()),
        }
        .render()
        .expect("mismatch show");
    }

    /// GET /invite?token=999.nope renders the "invalid" state without redirecting.
    #[tokio::test]
    async fn invalid_token_renders_invalid_copy() {
        let app = TestApp::new().await;
        factories::league(&app.pool, LeagueOptions::default()).await;

        let resp = app.get("/invite?token=999.nope").await;

        resp.assert_status_ok();
        let body = resp.text();
        assert!(
            body.contains("invalid or has expired"),
            "invalid copy: {body}"
        );
    }

    /// GET /invite?token=... when logged in as a different user renders
    /// mismatch information: shows both the signed-in email and the invited
    /// email, and hides the accept link.
    #[tokio::test]
    async fn shows_mismatch_for_wrong_account() {
        let app = TestApp::new().await;
        let gen_league = factories::league(&app.pool, LeagueOptions::default()).await;

        app.login_as(&gen_league.commish).await;

        let gen_invite = factories::invite(
            &app.pool,
            InviteOptions {
                league: Some(gen_league.league),
                ..Default::default()
            },
        )
        .await;
        let token = gen_invite.token();

        let resp = app.get(&format!("/invite?token={token}")).await;

        resp.assert_status_ok();
        let body = resp.text();
        assert!(
            body.contains(&gen_league.commish.email),
            "shows the currently signed-in email: {body}"
        );
        assert!(
            body.contains(&gen_invite.email),
            "shows the invited email: {body}"
        );
        assert!(
            !body.contains("/invite/accept?token="),
            "accept link must be hidden while mismatched: {body}"
        );
    }
}
