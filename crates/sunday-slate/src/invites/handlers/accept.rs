use crate::AppError;
use crate::auth::MaybeUser;
use crate::invites::extract::{Invitation, InviteState};
use crate::web::redirect;
use axum::response::{IntoResponse, Response};
use axum_htmx::HxRequest;
use axum_messages::Messages;
use tower_sessions::Session;

/// Pure routing decision for /invite/accept. Returns the flash message to
/// set, the location to redirect to, and the invite id to remember as
/// pending when the redirect goes straight to team creation.
fn accept_route(
    invite: &Invitation,
    user: Option<&crate::User>,
) -> (Option<String>, String, Option<i64>) {
    let token_param = invite.token_param();

    if invite.mismatched_email.is_some() {
        return (None, format!("/invite?token={token_param}"), None);
    }

    match &invite.invite_state {
        InviteState::Invalid => (None, format!("/invite?token={token_param}"), None),
        InviteState::ExistingUser(v) => match user {
            Some(_) => (None, "/teams/new".to_string(), Some(v.invite_id)),
            None => {
                let hub = format!("/invite?token={token_param}");
                let next = urlencoding::encode(&hub);
                (
                    Some("Log in to accept your invite.".to_string()),
                    format!("/login?next={next}"),
                    None,
                )
            }
        },
        InviteState::NewUser(_) => (
            None,
            format!("/invite/create-user?token={token_param}"),
            None,
        ),
    }
}

/// GET /invite/accept?token= — route the visitor to the next step by state.
pub async fn accept(
    invite: Invitation,
    MaybeUser(user): MaybeUser,
    HxRequest(is_htmx): HxRequest,
    session: Session,
    messages: Messages,
) -> Result<Response, AppError> {
    let (flash, location, pending) = accept_route(&invite, user.as_ref());
    if let Some(msg) = flash {
        messages.info(msg);
    }
    if let Some(invite_id) = pending {
        crate::invites::set_pending(&session, invite_id).await?;
    }
    Ok(redirect(is_htmx, location).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::invites::store::VerifiedInvite;
    use crate::tests::factories::{InviteOptions, UserOptions};

    use crate::tests::{TestApp, factories};

    use http::StatusCode;

    fn verified_invite(email: &str) -> VerifiedInvite {
        VerifiedInvite {
            invite_id: 1,
            league_id: 1,
            email: email.to_string(),
            league_name: "Test League".into(),
        }
    }

    fn test_invitation(state: InviteState, mismatched: Option<&str>) -> Invitation {
        Invitation {
            invite_state: state,
            token: "1.abc".to_string(),
            mismatched_email: mismatched.map(|s| s.to_string()),
        }
    }

    #[test]
    fn accept_route_mismatched_redirects_to_invite() {
        let v = verified_invite("a@b.com");
        let inv = test_invitation(InviteState::NewUser(v), Some("other@b.com"));
        let (flash, loc, pending) = accept_route(&inv, None);
        assert_eq!(flash, None);
        assert_eq!(loc, "/invite?token=1.abc");
        assert_eq!(pending, None);
    }

    #[test]
    fn accept_route_invalid_redirects_to_invite() {
        let inv = test_invitation(InviteState::Invalid, None);
        let (flash, loc, pending) = accept_route(&inv, None);
        assert_eq!(flash, None);
        assert_eq!(loc, "/invite?token=1.abc");
        assert_eq!(pending, None);
    }

    #[test]
    fn accept_route_existing_user_logged_in_redirects_to_teams_new() {
        let v = verified_invite("a@b.com");
        let inv = test_invitation(InviteState::ExistingUser(v), None);
        let user = crate::User {
            id: 1,
            email: "a@b.com".into(),
            password_hash: String::new(),
            is_admin: false,
            created_at: time::PrimitiveDateTime::MIN,
            updated_at: time::PrimitiveDateTime::MIN,
        };
        let (flash, loc, pending) = accept_route(&inv, Some(&user));
        assert_eq!(flash, None);
        assert_eq!(loc, "/teams/new");
        assert_eq!(pending, Some(1), "the accepted invite is remembered");
    }

    #[test]
    fn accept_route_existing_user_anonymous_redirects_to_login() {
        let v = verified_invite("a@b.com");
        let inv = test_invitation(InviteState::ExistingUser(v), None);
        let (flash, loc, pending) = accept_route(&inv, None);
        assert_eq!(pending, None);
        assert_eq!(flash, Some("Log in to accept your invite.".to_string()));
        assert!(
            loc.starts_with("/login?next="),
            "expected /login?next=..., got: {loc}"
        );
        let next_encoded = loc.strip_prefix("/login?next=").unwrap();
        let next = urlencoding::decode(next_encoded).unwrap();
        assert_eq!(next, "/invite?token=1.abc");
    }

    #[test]
    fn accept_route_new_user_redirects_to_create_user() {
        let v = verified_invite("a@b.com");
        let inv = test_invitation(InviteState::NewUser(v), None);
        let (flash, loc, pending) = accept_route(&inv, None);
        assert_eq!(flash, None);
        assert_eq!(loc, "/invite/create-user?token=1.abc");
        assert_eq!(
            pending, None,
            "create-user records the pending invite itself"
        );
    }

    /// GET /invite/accept?token= when logged in as the wrong user redirects
    /// back to the invite page.
    #[tokio::test]
    async fn accept_mismatched_redirects_to_invite() {
        let app = TestApp::new().await;
        let gen_user = factories::user(&app.pool, UserOptions::default()).await;

        app.login(&gen_user).await;

        let gen_invite = factories::invite(&app.pool, InviteOptions::default()).await;
        let token = gen_invite.token();

        let resp = app.get(&format!("/invite/accept?token={token}")).await;

        resp.assert_status(StatusCode::SEE_OTHER);
        let loc = resp.header("location");
        let loc = loc.to_str().unwrap();
        assert_eq!(
            loc,
            format!("/invite?token={}", urlencoding::encode(&token))
        );
    }

    /// GET /invite/accept?token= for an existing user, while not logged in,
    /// redirects to login with a `next=` param pointing back to the invite.
    #[tokio::test]
    async fn accept_existing_user_anonymous_redirects_to_login() {
        let app = TestApp::new().await;
        let gen_user = factories::user(&app.pool, UserOptions::default()).await;
        let gen_invite = factories::invite(
            &app.pool,
            InviteOptions {
                email: Some(gen_user.user.email),
                ..Default::default()
            },
        )
        .await;

        let token = gen_invite.token();

        let resp = app.get(&format!("/invite/accept?token={token}")).await;

        resp.assert_status(StatusCode::SEE_OTHER);
        let loc = resp.header("location");
        let loc = loc.to_str().unwrap();
        assert!(loc.starts_with("/login?next="), "loc: {loc}");
    }

    /// GET /invite/accept?token= when logged in as the matching user
    /// redirects directly to /teams/new (the accept_route behavior).
    #[tokio::test]
    async fn accept_matching_logged_in_user_routes_to_teams_new() {
        let app = TestApp::new().await;
        let gen_user = factories::user(&app.pool, UserOptions::default()).await;

        app.login(&gen_user).await;

        let gen_invite = factories::invite(
            &app.pool,
            InviteOptions {
                email: Some(gen_user.user.email),
                ..Default::default()
            },
        )
        .await;
        let token = gen_invite.token();

        let resp = app.get(&format!("/invite/accept?token={token}")).await;

        resp.assert_status(StatusCode::SEE_OTHER);
        assert_eq!(resp.header("location"), "/teams/new");
    }
}
