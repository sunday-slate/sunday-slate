use crate::fantasy_teams::CurrentTeam;
use crate::{AppState, Db, error::AppError, users::User};
use axum::response::{IntoResponse, Response};
use axum::{extract::FromRequestParts, http::request::Parts};
use axum_login::AuthSession;

pub struct MaybeUser(pub Option<User>);

impl FromRequestParts<AppState> for MaybeUser {
    type Rejection = Response;
    async fn from_request_parts(parts: &mut Parts, _state: &AppState) -> Result<Self, Response> {
        let auth = parts.extensions.get::<AuthSession<Db>>().ok_or_else(|| {
            AppError::Internal(String::from("auth layer not mounted")).into_response()
        })?;
        Ok(MaybeUser(auth.user().await))
    }
}

pub struct CurrentUser(pub User);

impl FromRequestParts<AppState> for CurrentUser {
    type Rejection = Response;
    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Response> {
        let MaybeUser(user) = MaybeUser::from_request_parts(parts, state).await?;
        Ok(CurrentUser(
            user.ok_or_else(|| AppError::Unauthorized.into_response())?,
        ))
    }
}

pub struct AdminUser(pub User);

impl FromRequestParts<AppState> for AdminUser {
    type Rejection = Response;
    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Response> {
        let CurrentUser(user) = CurrentUser::from_request_parts(parts, state).await?;
        if !user.is_admin {
            return Err(AppError::Forbidden.into_response());
        }
        Ok(AdminUser(user))
    }
}

pub struct Commissioner(pub User);

impl FromRequestParts<AppState> for Commissioner {
    type Rejection = Response;
    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Response> {
        let CurrentUser(user) = CurrentUser::from_request_parts(parts, state).await?;
        let CurrentTeam(team) = CurrentTeam::from_request_parts(parts, state).await?;
        if !team.is_commissioner {
            return Err(AppError::Forbidden.into_response());
        }
        Ok(Commissioner(user))
    }
}
