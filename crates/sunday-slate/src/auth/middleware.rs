//! Install bootstrap guard.
//!
//! Redirects a non-bypassed request to the first unmet precondition:
//! no users → `/setup`; authenticated with no league → `/leagues/new`;
//! authenticated with no team in the resolved league, when team creation is
//! theirs to do (`needs_team_creation`) → `/teams/new`.

use crate::auth::MaybeUser;
use crate::fantasy_teams::{MaybeTeam, store as fantasy_teams, team_creation_target};
use crate::leagues::{League, MaybeLeague};
use crate::users::store as users;
use crate::{AppState, Db, User};
use axum::extract::Request;
use axum::extract::State;
use axum::middleware::Next;
use axum::response::{IntoResponse, Redirect, Response};

fn is_bypass_path(req: &Request) -> bool {
    let path = req.uri().path();
    matches!(
        path,
        "/setup"
            | "/logout"
            | "/leagues/new"
            | "/leagues"
            | "/teams/new"
            | "/teams"
            | "/invite"
            | "/invite/accept"
            | "/invite/create-user"
    ) || path.starts_with("/static/")
        || path.starts_with("/_dev/")
        || crate::admin::is_admin_path(path)
}

async fn is_fresh_install(db: &Db) -> bool {
    users::exists(db.reader())
        .await
        .is_ok_and(|users_exist| !users_exist)
}

/// Redirect to the first unmet bootstrap precondition.
pub async fn bootstrap(
    MaybeUser(user): MaybeUser,
    MaybeLeague(league): MaybeLeague,
    MaybeTeam(team): MaybeTeam,
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Response {
    if is_bypass_path(&req) {
        return next.run(req).await;
    }

    if user.is_none() {
        if is_fresh_install(&state.db).await {
            return Redirect::to("/setup").into_response();
        }
        return next.run(req).await;
    }

    if league.is_none() {
        return Redirect::to("/leagues/new").into_response();
    }

    if team.is_none() && needs_team_creation(&state.db, &league.unwrap(), &user.unwrap()).await {
        return Redirect::to("/teams/new").into_response();
    }

    next.run(req).await
}

/// Whether a user with no team in the resolved league must be sent to
/// `/teams/new`: the admin bootstrapping a league that has no teams yet, or
/// a user with no team in any league who holds an active invite. Members of
/// healthy leagues are never bounced by someone else's empty league.
/// Delegating to `team_creation_target` keeps bounce ⇒ creation-allowed, so
/// the guard cannot loop with `TeamCreator`'s redirect home.
async fn needs_team_creation(db: &Db, league: &League, user: &User) -> bool {
    // The gate: is the missing team this user's to create?
    let mine = if user.is_admin {
        !fantasy_teams::has_any(db.reader(), league.id)
            .await
            .unwrap_or(true)
    } else {
        !fantasy_teams::any_for_user(db.reader(), user.id)
            .await
            .unwrap_or(true)
    };
    // No pending invite or league here: the guard only cares whether creation
    // is possible, and any active invite answers that.
    mine && team_creation_target(db, Some(league.clone()), None, None, user)
        .await
        .is_ok_and(|target| target.is_some())
}
