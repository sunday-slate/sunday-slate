use crate::User;
use crate::auth::MaybeUser;
use crate::invites::store as invites;
use crate::invites::store::VerifiedInvite;
use crate::users::store as users;
use crate::web::{is_htmx, redirect};
use crate::{AppError, AppState};
use axum::extract::{FromRequestParts, Query};
use axum::http::request::Parts;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

#[derive(Deserialize)]
pub struct TokenQuery {
    pub(crate) token: Option<String>,
}

pub enum InviteState {
    Invalid,
    ExistingUser(VerifiedInvite),
    NewUser(VerifiedInvite),
}

/// `Some(current_user's email)` when someone is logged in whose email doesn't
/// match the invite's target email.
pub(crate) fn mismatched_email(
    current_user: Option<&User>,
    invite_state: &InviteState,
) -> Option<String> {
    let target_email = match invite_state {
        InviteState::Invalid => return None,
        InviteState::ExistingUser(v) | InviteState::NewUser(v) => &v.email,
    };
    current_user
        .filter(|u| &u.email != target_email)
        .map(|u| u.email.clone())
}

async fn invite_state(state: &AppState, raw: &str) -> Result<InviteState, AppError> {
    match invites::verify(&state.db, raw).await? {
        None => Ok(InviteState::Invalid),
        Some(verified_invite) => {
            match users::find_by_email(state.db.reader(), &verified_invite.email).await? {
                Some(_) => Ok(InviteState::ExistingUser(verified_invite)),
                None => Ok(InviteState::NewUser(verified_invite)),
            }
        }
    }
}

pub struct Invitation {
    pub invite_state: InviteState,
    pub token: String,
    pub mismatched_email: Option<String>,
}

impl Invitation {
    pub fn token_param(&self) -> String {
        urlencoding::encode(&self.token).to_string()
    }

    async fn resolve(state: &AppState, raw: &str) -> Result<Invitation, AppError> {
        Ok(Invitation {
            invite_state: invite_state(state, raw).await?,
            token: raw.to_string(),
            mismatched_email: None,
        })
    }
}

impl FromRequestParts<AppState> for Invitation {
    type Rejection = Response;
    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Response> {
        let Query(q) = Query::<TokenQuery>::from_request_parts(parts, state)
            .await
            .map_err(IntoResponse::into_response)?;
        let raw = q.token.unwrap_or_default();
        let MaybeUser(current_user) = MaybeUser::from_request_parts(parts, state).await?;

        let mut invitation = Invitation::resolve(state, &raw)
            .await
            .map_err(IntoResponse::into_response)?;
        invitation.mismatched_email =
            mismatched_email(current_user.as_ref(), &invitation.invite_state);
        Ok(invitation)
    }
}

#[derive(Debug)]
pub struct NewInvitee {
    pub invite: VerifiedInvite,
    pub token: String, // raw token, for the hidden form field
}

impl NewInvitee {
    /// Resolve a raw token to a new invitee, redirecting on any other state
    /// (including when `current_user` is logged in as a non-matching email).
    /// For handlers that read the token from the body rather than the query.
    pub async fn resolve(
        is_htmx: bool,
        state: &AppState,
        raw: &str,
        current_user: Option<&User>,
    ) -> Result<NewInvitee, Box<Response>> {
        let mut invitation = Invitation::resolve(state, raw)
            .await
            .map_err(|error| Box::new(error.into_response()))?;
        invitation.mismatched_email = mismatched_email(current_user, &invitation.invite_state);
        to_new_invitee(is_htmx, invitation)
    }
}

impl FromRequestParts<AppState> for NewInvitee {
    type Rejection = Response;
    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Response> {
        let is_htmx = is_htmx(&parts.headers);
        let invitation = Invitation::from_request_parts(parts, state).await?;
        to_new_invitee(is_htmx, invitation).map_err(|response| *response)
    }
}

fn to_new_invitee(is_htmx: bool, invitation: Invitation) -> Result<NewInvitee, Box<Response>> {
    let token_param = invitation.token_param();

    if invitation.mismatched_email.is_some() {
        return Err(Box::new(redirect(
            is_htmx,
            format!("/invite?token={token_param}"),
        )));
    }

    match invitation.invite_state {
        InviteState::Invalid => Err(Box::new(redirect(
            is_htmx,
            format!("/invite?token={token_param}"),
        ))),
        InviteState::ExistingUser(_) => Err(Box::new(redirect(
            is_htmx,
            format!("/invite/accept?token={token_param}"),
        ))),
        InviteState::NewUser(v) => Ok(NewInvitee {
            invite: v,
            token: invitation.token,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::invites::store::VerifiedInvite;
    use axum::http::StatusCode;

    fn test_user(email: &str) -> crate::User {
        crate::User {
            id: 1,
            email: email.to_string(),
            password_hash: String::new(),
            is_admin: false,
            created_at: time::PrimitiveDateTime::MIN,
            updated_at: time::PrimitiveDateTime::MIN,
        }
    }

    fn verified_invite(email: &str) -> VerifiedInvite {
        VerifiedInvite {
            invite_id: 1,
            league_id: 1,
            email: email.to_string(),
            league_name: "Test League".into(),
        }
    }

    // —— mismatched_email ——

    #[test]
    fn mismatched_email_no_user_returns_none() {
        let state = InviteState::NewUser(verified_invite("a@b.com"));
        assert_eq!(mismatched_email(None, &state), None);
    }

    #[test]
    fn mismatched_email_matching_user_returns_none() {
        let user = test_user("a@b.com");
        let state = InviteState::NewUser(verified_invite("a@b.com"));
        assert_eq!(mismatched_email(Some(&user), &state), None);
    }

    #[test]
    fn mismatched_email_different_user_returns_email() {
        let user = test_user("other@b.com");
        let state = InviteState::NewUser(verified_invite("a@b.com"));
        assert_eq!(
            mismatched_email(Some(&user), &state),
            Some("other@b.com".to_string())
        );
    }

    #[test]
    fn mismatched_email_invalid_state_returns_none() {
        let user = test_user("a@b.com");
        assert_eq!(mismatched_email(Some(&user), &InviteState::Invalid), None);
    }

    // —— to_new_invitee ——

    fn err_location(response: &Response) -> Option<String> {
        if response.status() == StatusCode::OK {
            // HxRedirect puts the location in the HX-Redirect header
            response
                .headers()
                .get("HX-Redirect")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string())
        } else {
            response
                .headers()
                .get("location")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string())
        }
    }

    #[test]
    fn to_new_invitee_mismatched_redirects_to_invite() {
        let v = verified_invite("a@b.com");
        let invitation = Invitation {
            invite_state: InviteState::NewUser(v),
            token: "1.abc".into(),
            mismatched_email: Some("other@b.com".into()),
        };
        let resp = *to_new_invitee(false, invitation).unwrap_err();
        assert_eq!(resp.status(), StatusCode::SEE_OTHER);
        assert_eq!(err_location(&resp).as_deref(), Some("/invite?token=1.abc"));
    }

    #[test]
    fn to_new_invitee_invalid_redirects_to_invite() {
        let invitation = Invitation {
            invite_state: InviteState::Invalid,
            token: "1.abc".into(),
            mismatched_email: None,
        };
        let resp = *to_new_invitee(false, invitation).unwrap_err();
        assert_eq!(resp.status(), StatusCode::SEE_OTHER);
        assert_eq!(err_location(&resp).as_deref(), Some("/invite?token=1.abc"));
    }

    #[test]
    fn to_new_invitee_existing_user_redirects_to_accept() {
        let v = verified_invite("a@b.com");
        let invitation = Invitation {
            invite_state: InviteState::ExistingUser(v),
            token: "1.abc".into(),
            mismatched_email: None,
        };
        let resp = *to_new_invitee(false, invitation).unwrap_err();
        assert_eq!(resp.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            err_location(&resp).as_deref(),
            Some("/invite/accept?token=1.abc")
        );
    }

    #[test]
    fn to_new_invitee_existing_user_htmx_redirect() {
        let v = verified_invite("a@b.com");
        let invitation = Invitation {
            invite_state: InviteState::ExistingUser(v),
            token: "1.abc".into(),
            mismatched_email: None,
        };
        let resp = *to_new_invitee(true, invitation).unwrap_err();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            err_location(&resp).as_deref(),
            Some("/invite/accept?token=1.abc")
        );
    }

    #[test]
    fn to_new_invitee_new_user_returns_ok() {
        let v = verified_invite("a@b.com");
        let invitation = Invitation {
            invite_state: InviteState::NewUser(v),
            token: "1.abc".into(),
            mismatched_email: None,
        };
        let result = to_new_invitee(false, invitation).unwrap();
        assert_eq!(result.invite.email, "a@b.com");
        assert_eq!(result.token, "1.abc");
    }
}
