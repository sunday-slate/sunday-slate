use crate::auth::CurrentUser;
use crate::fantasy_teams::FantasyTeam;
use crate::fantasy_teams::store as fantasy_teams;
use crate::invites::store as invites;
use crate::leagues::store as leagues_store;
use crate::leagues::{League, MaybeLeague};
use crate::{AppError, AppState, Db, User};
use axum::extract::{FromRequestParts, Path};
use axum::http::request::Parts;
use axum::response::{IntoResponse, Redirect, Response};
use tower_sessions::Session;

pub struct NoTeam;

impl FromRequestParts<AppState> for NoTeam {
    type Rejection = Response;
    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Response> {
        let MaybeTeam(team) = MaybeTeam::from_request_parts(parts, state).await?;
        match team {
            Some(_) => Err(AppError::Unauthorized.into_response()),
            None => Ok(NoTeam),
        }
    }
}

/// The signed-in user's team in the resolved league, read from the request
/// extension cached by the `leagues::resolve` middleware. `None` when signed
/// out, teamless in the league, or when the middleware did not run.
pub struct MaybeTeam(pub Option<FantasyTeam>);

impl FromRequestParts<AppState> for MaybeTeam {
    type Rejection = Response;
    async fn from_request_parts(parts: &mut Parts, _state: &AppState) -> Result<Self, Response> {
        Ok(MaybeTeam(
            parts
                .extensions
                .get::<Option<FantasyTeam>>()
                .cloned()
                .flatten(),
        ))
    }
}

/// The current user's team in the current league. A guard for team-scoped
/// pages: absence rejects with `Unauthorized`. Onboarding a user who has no
/// team is the bootstrap middleware's job — it redirects to `/teams/new`
/// before a team-scoped handler runs.
pub struct CurrentTeam(pub FantasyTeam);

impl FromRequestParts<AppState> for CurrentTeam {
    type Rejection = Response;
    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Response> {
        let MaybeTeam(team) = MaybeTeam::from_request_parts(parts, state).await?;
        Ok(CurrentTeam(
            team.ok_or_else(|| AppError::Unauthorized.into_response())?,
        ))
    }
}

/// The current user's team, when it is the team named by the route's `{id}`.
/// Any other id — including a viewer with no team in the resolved league,
/// such as the site admin visiting another league — is `NotFound`.
pub struct OwnedTeam(pub FantasyTeam);

impl FromRequestParts<AppState> for OwnedTeam {
    type Rejection = Response;
    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Response> {
        let Path(id) = Path::<i64>::from_request_parts(parts, state)
            .await
            .map_err(IntoResponse::into_response)?;
        let MaybeTeam(team) = MaybeTeam::from_request_parts(parts, state).await?;
        match team {
            Some(team) if team.id == id => Ok(OwnedTeam(team)),
            _ => Err(AppError::NotFound.into_response()),
        }
    }
}

/// Resolve where `user` may create a team, and the invite consumed by doing
/// so (`None` invite for the site admin's commissioner bootstrap). The single
/// source of truth for team-creation authorization, shared by the bootstrap
/// guard and the `TeamCreator` extractor.
///
/// The admin creates in the league they just created (`pending_league`,
/// session-carried), else the ambient (resolved) league. A non-admin creates
/// in the league of the invite they accepted (`pending_invite`,
/// session-carried, ignored when no longer active for their email), else
/// their most recent active invite. Someone who already has a team in the
/// target league gets `Ok(None)`, as does a non-admin with no active invite.
pub async fn team_creation_target(
    db: &Db,
    ambient: Option<League>,
    pending_invite: Option<i64>,
    pending_league: Option<i64>,
    user: &User,
) -> Result<Option<(League, Option<i64>)>, AppError> {
    let (league, invite_id) = if user.is_admin {
        let league = match pending_league {
            Some(id) => leagues_store::by_id(db.reader(), id).await?.or(ambient),
            None => ambient,
        };
        let Some(league) = league else {
            return Ok(None);
        };
        (league, None)
    } else {
        let invite = match pending_invite {
            Some(id) => invites::find_active_by_id(db.reader(), id, &user.email).await?,
            None => None,
        };
        let invite = match invite {
            Some(invite) => Some(invite),
            None => invites::find_active_any(db.reader(), &user.email).await?,
        };
        let Some(invite) = invite else {
            return Ok(None);
        };
        let league = leagues_store::by_id(db.reader(), invite.league_id)
            .await?
            .expect("invites.league_id is a foreign key into leagues");
        (league, Some(invite.id))
    };

    if fantasy_teams::team_for_user(db.reader(), league.id, user.id)
        .await?
        .is_some()
    {
        return Ok(None);
    }
    Ok(Some((league, invite_id)))
}

pub struct TeamCreator {
    pub user: User,
    pub league: League,
    pub invite_id: Option<i64>,
}

impl FromRequestParts<AppState> for TeamCreator {
    type Rejection = Response;
    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Response> {
        let MaybeLeague(ambient) = MaybeLeague::from_request_parts(parts, state).await?;
        let CurrentUser(user) = CurrentUser::from_request_parts(parts, state).await?;
        let (pending_invite, pending_league) = match Session::from_request_parts(parts, state).await
        {
            Ok(session) => (
                crate::invites::pending(&session).await,
                crate::leagues::pending_creation(&session).await,
            ),
            Err(_) => (None, None),
        };

        match team_creation_target(&state.db, ambient, pending_invite, pending_league, &user)
            .await
            .map_err(|e| e.into_response())?
        {
            Some((league, invite_id)) => Ok(TeamCreator {
                user,
                league,
                invite_id,
            }),
            // Already has a team (or no invite) — nothing to create; send home.
            None => Err(Redirect::to("/").into_response()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::leagues::store as leagues;
    use crate::users::store as users;
    use sqlx::SqlitePool;

    async fn seed_league(pool: &SqlitePool) -> League {
        leagues::create(pool, "Sunday Funday")
            .await
            .expect("create league")
    }

    async fn make_user(pool: &SqlitePool, email: &str, is_admin: bool) -> User {
        users::create(pool, email, "hash", is_admin)
            .await
            .expect("create user")
    }

    async fn give_team(pool: &SqlitePool, league_id: i64, user: &User) {
        fantasy_teams::create(pool, league_id, user.id, "Champs", "Owner", user.is_admin)
            .await
            .expect("create team");
    }

    async fn give_invite(pool: &SqlitePool, league_id: i64, email: &str) -> i64 {
        invites::upsert(pool, league_id, email, "token-hash")
            .await
            .expect("create invite")
    }

    #[sqlx::test]
    async fn admin_without_team_and_ambient_league_creates_without_invite(pool: SqlitePool) {
        let league = seed_league(&pool).await;
        let admin = make_user(&pool, "admin@test.local", true).await;
        let db = Db::test(pool.clone());
        let target = team_creation_target(&db, Some(league.clone()), None, None, &admin)
            .await
            .expect("target")
            .expect("some");
        assert_eq!(target.0.id, league.id);
        assert_eq!(target.1, None, "admin bootstrap consumes no invite");
    }

    #[sqlx::test]
    async fn admin_with_team_in_ambient_league_is_none(pool: SqlitePool) {
        let league = seed_league(&pool).await;
        let admin = make_user(&pool, "admin@test.local", true).await;
        give_team(&pool, league.id, &admin).await;
        let db = Db::test(pool.clone());
        let target = team_creation_target(&db, Some(league), None, None, &admin)
            .await
            .expect("target");
        assert!(
            target.is_none(),
            "an admin who already has a team cannot create another"
        );
    }

    #[sqlx::test]
    async fn invitee_with_active_invite_targets_the_invite_league(pool: SqlitePool) {
        let league = seed_league(&pool).await;
        let invitee = make_user(&pool, "invitee@test.local", false).await;
        let invite_id = give_invite(&pool, league.id, "invitee@test.local").await;
        let db = Db::test(pool.clone());
        // The invite carries the league, so the ambient league doesn't matter.
        let target = team_creation_target(&db, None, None, None, &invitee)
            .await
            .expect("target")
            .expect("some");
        assert_eq!(target.0.id, league.id);
        assert_eq!(target.1, Some(invite_id));
    }

    #[sqlx::test]
    async fn non_admin_without_invite_is_none(pool: SqlitePool) {
        let league = seed_league(&pool).await;
        let stranger = make_user(&pool, "stranger@test.local", false).await;
        let db = Db::test(pool.clone());
        let target = team_creation_target(&db, Some(league), None, None, &stranger)
            .await
            .expect("target");
        assert!(target.is_none());
    }

    #[sqlx::test]
    async fn non_admin_with_team_in_invite_league_is_none(pool: SqlitePool) {
        let league = seed_league(&pool).await;
        let member = make_user(&pool, "member@test.local", false).await;
        give_team(&pool, league.id, &member).await;
        give_invite(&pool, league.id, "member@test.local").await;
        let db = Db::test(pool.clone());
        let target = team_creation_target(&db, Some(league), None, None, &member)
            .await
            .expect("target");
        assert!(target.is_none());
    }

    #[sqlx::test]
    async fn invitee_target_is_the_invite_league_not_the_ambient(pool: SqlitePool) {
        let league_a = leagues::create(&pool, "League A").await.expect("league");
        let league_b = leagues::create(&pool, "League B").await.expect("league");
        let invitee = make_user(&pool, "invitee@test.local", false).await;
        let invite_id = give_invite(&pool, league_b.id, "invitee@test.local").await;
        let db = Db::test(pool.clone());
        let target = team_creation_target(&db, Some(league_a), None, None, &invitee)
            .await
            .expect("target")
            .expect("some");
        assert_eq!(target.0.id, league_b.id);
        assert_eq!(target.1, Some(invite_id));
    }

    #[sqlx::test]
    async fn pending_invite_overrides_the_newest_invite(pool: SqlitePool) {
        let league_a = leagues::create(&pool, "League A").await.expect("league");
        let league_b = leagues::create(&pool, "League B").await.expect("league");
        let invitee = make_user(&pool, "invitee@test.local", false).await;
        let older = give_invite(&pool, league_a.id, "invitee@test.local").await;
        sqlx::query(
            "UPDATE invites SET created_at = datetime('now','subsec','-1 hour') WHERE id = ?1",
        )
        .bind(older)
        .execute(&pool)
        .await
        .expect("age invite");
        give_invite(&pool, league_b.id, "invitee@test.local").await;
        let db = Db::test(pool.clone());
        let target = team_creation_target(&db, None, Some(older), None, &invitee)
            .await
            .expect("target")
            .expect("some");
        assert_eq!(target.0.id, league_a.id);
        assert_eq!(target.1, Some(older));
    }

    #[sqlx::test]
    async fn stale_pending_invite_falls_back_to_the_newest(pool: SqlitePool) {
        let league_a = leagues::create(&pool, "League A").await.expect("league");
        let league_b = leagues::create(&pool, "League B").await.expect("league");
        let invitee = make_user(&pool, "invitee@test.local", false).await;
        let older = give_invite(&pool, league_a.id, "invitee@test.local").await;
        sqlx::query(
            "UPDATE invites SET created_at = datetime('now','subsec','-1 hour') WHERE id = ?1",
        )
        .bind(older)
        .execute(&pool)
        .await
        .expect("age invite");
        let newer = give_invite(&pool, league_b.id, "invitee@test.local").await;
        invites::mark_accepted(&pool, older).await.expect("accept");
        let db = Db::test(pool.clone());
        let target = team_creation_target(&db, None, Some(older), None, &invitee)
            .await
            .expect("target")
            .expect("some");
        assert_eq!(target.0.id, league_b.id);
        assert_eq!(target.1, Some(newer));
    }
}
