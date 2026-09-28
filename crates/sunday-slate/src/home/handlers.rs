use axum::extract::State;
use axum::response::Redirect;

use crate::home::service;
use crate::{AppError, AppState};

/// `GET /` never renders: it redirects to the Current Contest, or the
/// standings when the season has none. Onboarding (setup, league, team
/// creation) is handled upstream by `auth::bootstrap`.
pub async fn index(State(state): State<AppState>) -> Result<Redirect, AppError> {
    let to = match service::current_contest_id(&state).await? {
        Some(id) => format!("/contests/{}", id.0),
        None => "/standings".to_string(),
    };
    Ok(Redirect::to(&to))
}
