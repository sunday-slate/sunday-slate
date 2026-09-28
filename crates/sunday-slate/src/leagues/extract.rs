use crate::leagues::League;
use crate::{AppError, AppState};
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::response::{IntoResponse, Response};

/// The league resolved for this request by the `leagues::resolve` middleware,
/// read from request extensions. `None` when no league could be resolved, or
/// when the middleware did not run (routes mounted outside it).
pub struct MaybeLeague(pub Option<League>);

impl FromRequestParts<AppState> for MaybeLeague {
    type Rejection = Response;
    async fn from_request_parts(parts: &mut Parts, _state: &AppState) -> Result<Self, Response> {
        Ok(MaybeLeague(
            parts.extensions.get::<Option<League>>().cloned().flatten(),
        ))
    }
}

/// The league resolved for this request by the `leagues::resolve` middleware:
/// an entity route's owning league, the caller's sticky session choice, or
/// the fallback league, in that order — always a league the caller may use.
/// Absence rejects with `AppError::Unauthorized`.
pub struct CurrentLeague(pub League);

impl FromRequestParts<AppState> for CurrentLeague {
    type Rejection = Response;
    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Response> {
        let MaybeLeague(league) = MaybeLeague::from_request_parts(parts, state).await?;
        Ok(CurrentLeague(
            league.ok_or_else(|| AppError::Unauthorized.into_response())?,
        ))
    }
}
