use super::resolve::{set_active, usable_league};
use crate::auth::{AdminUser, CurrentUser};
use crate::leagues::store as leagues;
use crate::validation::{normalize_name, valid_name_chars};
use crate::web::{FormErrors, FormInput, Valid, redirect};
use crate::{AppError, AppState, form_view};
use askama::Template;
use axum::Form;
use axum::extract::State;
use axum::response::{Html, IntoResponse, Response};
use axum_htmx::HxRequest;
use garde::Validate;
use serde::Deserialize;
use tower_sessions::Session;

#[derive(Template, Default)]
#[template(path = "leagues/new.html", blocks = ["form"])]
pub struct NewLeagueTemplate {
    errors: FormErrors,
    name: String,
}

form_view!(NewLeagueTemplate);

#[derive(Deserialize, Validate)]
pub struct LeagueInput {
    #[garde(length(min = 1, max = 80), custom(valid_name_chars))]
    name: String,
}

impl FormInput for LeagueInput {
    type View = NewLeagueTemplate;
    type Ctx = ();

    fn normalize(&mut self) {
        self.name = normalize_name(&self.name);
    }

    fn to_view(&self, (): (), errors: FormErrors) -> Self::View {
        NewLeagueTemplate {
            errors,
            name: self.name.clone(),
        }
    }
}

pub async fn new(_: AdminUser) -> Result<impl IntoResponse, AppError> {
    Ok(Html(NewLeagueTemplate::default().render()?).into_response())
}

pub async fn create(
    _: AdminUser,
    HxRequest(is_htmx): HxRequest,
    session: Session,
    State(state): State<AppState>,
    Valid(form): Valid<LeagueInput>,
) -> Result<impl IntoResponse, AppError> {
    let league = state
        .db
        .write_tx(async |conn| -> Result<_, sqlx::Error> {
            leagues::create(&mut *conn, &form.name).await
        })
        .await?;

    // Carry the new league to /teams/new in the session; league resolution
    // rejects leagues where the user has no team.
    super::set_pending_creation(&session, league.id).await?;

    Ok(redirect(is_htmx, "/teams/new"))
}

#[derive(Deserialize)]
pub struct SwitchInput {
    league_id: i64,
}

/// POST /leagues/switch — set the session's active league. Allowed only for
/// users with a team in the league, admin included.
pub async fn switch(
    CurrentUser(user): CurrentUser,
    session: Session,
    HxRequest(is_htmx): HxRequest,
    State(state): State<AppState>,
    Form(input): Form<SwitchInput>,
) -> Result<Response, AppError> {
    if usable_league(&state, &user, input.league_id)
        .await?
        .is_none()
    {
        return Err(AppError::Forbidden);
    }
    set_active(&session, input.league_id).await?;
    Ok(redirect(is_htmx, "/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_and_fragment_render() {
        NewLeagueTemplate::default().render().expect("page");
        NewLeagueTemplate {
            errors: FormErrors::default(),
            name: "Sunday Funday".to_string(),
        }
        .as_form()
        .render()
        .expect("fragment");
    }

    #[test]
    fn form_accepts_valid_and_accented() {
        for name in ["Sunday Funday", "4th & Long", "José's Heroes"] {
            let form = LeagueInput {
                name: name.to_string(),
            };
            assert!(form.validate().is_ok(), "{name} should be valid");
        }
    }

    #[test]
    fn form_rejects_empty_and_emoji_and_symbols() {
        for name in ["", "🏈Team🏈", "★Champs★"] {
            let form = LeagueInput {
                name: name.to_string(),
            };
            assert!(form.validate().is_err(), "{name:?} should be rejected");
        }
    }
}
