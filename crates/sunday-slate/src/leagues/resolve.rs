//! Per-request league resolution.
//!
//! Resolves which league a request is about — and the user's team there —
//! and caches both in request extensions, where `MaybeLeague` and `MaybeTeam`
//! read them. Mounted between the auth layer and `auth::bootstrap`, so the
//! guard, `request_ctx`, and every handler see the same league. The ladder:
//!
//! 1. Entity routes (`ENTITY_ROUTES`): the league the addressed entity
//!    belongs to, when the user can use that league. Sticky — writes the
//!    session's active league.
//! 2. Ambient routes: the session's active league, when it still exists and
//!    the user can still use it. A stale value is cleared.
//! 3. Fallback: the user's lowest-id team league, else the first league
//!    (bootstrap and teamless users).
//!
//! Every user, admin included, can use a league only where they have a
//! team.

use crate::auth::MaybeUser;
use crate::entries::store as entries;
use crate::fantasy_teams::FantasyTeam;
use crate::fantasy_teams::store as fantasy_teams;
use crate::invites::store as invites;
use crate::leagues::{League, store as leagues};
use crate::{AppError, AppState, User};
use axum::extract::{MatchedPath, RawPathParams, Request, State};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use tower_sessions::Session;

/// Session key holding the signed-in user's active league id.
const ACTIVE_LEAGUE_KEY: &str = "active_league_id";

/// Set as a request extension when an entity link silently switched the
/// session's active league — the shell shows a "Now viewing …" toast.
#[derive(Clone)]
pub struct LeagueSwitched(pub String);

/// Make `league_id` the session's active league.
pub(crate) async fn set_active(session: &Session, league_id: i64) -> Result<(), AppError> {
    session.insert(ACTIVE_LEAGUE_KEY, league_id).await?;
    Ok(())
}

/// The kind of entity an entity route addresses, which determines how its
/// `{id}` resolves to a league.
#[derive(Clone, Copy)]
enum Entity {
    Entry,
    Team,
    Invite,
}

/// Route patterns whose `{id}` param names an entity with an inherent league.
const ENTITY_ROUTES: &[(&str, Entity)] = &[
    ("/entries/{id}", Entity::Entry),
    ("/entries/{id}/events", Entity::Entry),
    ("/teams/{id}", Entity::Team),
    ("/teams/{id}/edit", Entity::Team),
    ("/teams/{id}/logo", Entity::Team),
    ("/invites/{id}/link", Entity::Invite),
    ("/invites/{id}/revoke", Entity::Invite),
];

/// Id-carrying route patterns that deliberately have no inherent league:
/// contests and their draft-entry flow are global slates viewed through the
/// ambient league; admin routes are global. Grown by the tripwire test in
/// this module whenever a new id route is added.
#[cfg_attr(not(test), allow(dead_code))]
const AMBIENT_ID_ROUTES: &[&str] = &[
    // Test/dev-only infrastructure, not compiled or mounted in production.
    "/__test/login/{user_id}",
    "/_dev/mail/{idx}",
    "/contests/{id}",
    "/contests/{id}/events",
    "/contests/{id}/draft-entry/discard",
    "/contests/{id}/draft-entry",
    "/contests/{id}/draft-entry/save",
    "/contests/{id}/draft-entry/{slot}",
    "/contests/{id}/draft-entry/{slot}/clear",
    // Admin salary-import tooling: global, gated by `AdminUser`.
    "/{id}",
    "/{id}/commit",
    "/{id}/discard",
    "/{id}/resolved",
    "/{id}/rows/{row_id}",
    "/{id}/rows/{row_id}/players",
    "/{id}/rows/{row_id}/undo",
    "/{id}/skipped",
    "/{id}/slate",
    "/{id}/triage",
];

/// Resolve the request's league — and the user's team in it — and cache both
/// in request extensions.
pub async fn resolve(
    MaybeUser(user): MaybeUser,
    session: Session,
    params: RawPathParams,
    State(state): State<AppState>,
    mut req: Request,
    next: Next,
) -> Response {
    // Cloned out: holding `&Request` across an await would make this future
    // non-`Send` (`Body` is not `Sync`).
    let matched = req.extensions().get::<MatchedPath>().cloned();
    let id = params
        .iter()
        .find(|(name, _)| *name == "id")
        .and_then(|(_, value)| value.parse::<i64>().ok());

    let (resolved, switched) = match resolve_league(
        &state,
        user.as_ref(),
        &session,
        matched.as_ref().map(MatchedPath::as_str),
        id,
    )
    .await
    {
        Ok(pair) => pair,
        Err(e) => return e.into_response(),
    };
    let (league, team) = resolved.map_or((None, None), |(l, t)| (Some(l), t));
    req.extensions_mut().insert(league);
    req.extensions_mut().insert(team);
    if let Some(switched) = switched {
        req.extensions_mut().insert(switched);
    }
    next.run(req).await
}

async fn resolve_league(
    state: &AppState,
    user: Option<&User>,
    session: &Session,
    matched: Option<&str>,
    id: Option<i64>,
) -> Result<
    (
        Option<(League, Option<FantasyTeam>)>,
        Option<LeagueSwitched>,
    ),
    AppError,
> {
    let Some(user) = user else {
        return Ok((None, None));
    };
    let reader = state.db.reader();

    // Rung 1: entity routes, sticky. Switching an already-set league is
    // surfaced as a toast; a first-time default is not a switch.
    if let Some(league_id) = entity_league_id(state, matched, id).await?
        && let Some(resolved) = usable_league(state, user, league_id).await?
    {
        let stored: Option<i64> = session.get(ACTIVE_LEAGUE_KEY).await?;
        if stored != Some(league_id) {
            set_active(session, league_id).await?;
        }
        let switched = (stored.is_some() && stored != Some(league_id))
            .then(|| LeagueSwitched(resolved.0.name.clone()));
        return Ok((Some(resolved), switched));
    }

    // Rung 2: the session's active league, validated.
    if let Some(league_id) = session.get::<i64>(ACTIVE_LEAGUE_KEY).await? {
        if let Some(resolved) = usable_league(state, user, league_id).await? {
            return Ok((Some(resolved), None));
        }
        session.remove::<i64>(ACTIVE_LEAGUE_KEY).await?;
    }

    // Rung 3: fallback.
    if let Some(league) = leagues::for_user(reader, user.id).await?.into_iter().next() {
        let team = fantasy_teams::team_for_user(reader, league.id, user.id).await?;
        return Ok((Some((league, team)), None));
    }
    Ok((
        leagues::first(reader).await?.map(|league| (league, None)),
        None,
    ))
}

/// The league behind `league_id` when `user` may use it, with their team
/// there. Every user, admin included, needs a team in the league. `None` for
/// an unusable or unknown league.
pub(crate) async fn usable_league(
    state: &AppState,
    user: &User,
    league_id: i64,
) -> Result<Option<(League, Option<FantasyTeam>)>, AppError> {
    let reader = state.db.reader();
    let team = fantasy_teams::team_for_user(reader, league_id, user.id).await?;
    if team.is_none() {
        return Ok(None);
    }
    Ok(leagues::by_id(reader, league_id)
        .await?
        .map(|league| (league, team)))
}

/// The league inherent to this request's entity, when the matched route is an
/// entity route. `None` for ambient routes, unknown ids, and non-numeric ids
/// (the handler 404s those).
async fn entity_league_id(
    state: &AppState,
    matched: Option<&str>,
    id: Option<i64>,
) -> Result<Option<i64>, AppError> {
    let entity = matched.and_then(|m| {
        ENTITY_ROUTES
            .iter()
            .find(|(pattern, _)| *pattern == m)
            .map(|(_, entity)| *entity)
    });
    let (Some(entity), Some(id)) = (entity, id) else {
        return Ok(None);
    };
    let reader = state.db.reader();
    Ok(match entity {
        Entity::Entry => entries::league_id_of(reader, id).await?,
        Entity::Team => fantasy_teams::league_id_of(reader, id).await?,
        Entity::Invite => invites::league_id_of(reader, id).await?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every id-carrying route pattern in the crate must be classified as an
    /// entity route (inherent league) or an ambient id route (deliberately
    /// global). A new `{id}` route that is neither fails here and forces the
    /// decision.
    #[test]
    fn every_id_route_is_classified() {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut patterns = std::collections::BTreeSet::new();
        collect_route_patterns(&src, &mut patterns);
        let unclassified: Vec<_> = patterns
            .iter()
            .filter(|p| p.contains('{'))
            .filter(|p| {
                !ENTITY_ROUTES
                    .iter()
                    .any(|(pattern, _)| *pattern == p.as_str())
                    && !AMBIENT_ID_ROUTES.contains(&p.as_str())
            })
            .collect();
        assert!(
            unclassified.is_empty(),
            "unclassified id routes {unclassified:?}: add each to ENTITY_ROUTES \
             (inherent league) or AMBIENT_ID_ROUTES (global/ambient) in leagues/resolve.rs"
        );
    }

    fn collect_route_patterns(dir: &std::path::Path, out: &mut std::collections::BTreeSet<String>) {
        for entry in std::fs::read_dir(dir).expect("read src dir") {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                collect_route_patterns(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let content = std::fs::read_to_string(&path).expect("read source file");
                let mut rest = content.as_str();
                while let Some(pos) = rest.find(".route(") {
                    rest = &rest[pos + ".route(".len()..];
                    let trimmed = rest.trim_start();
                    if let Some(stripped) = trimmed.strip_prefix('"')
                        && let Some(end) = stripped.find('"')
                        && !stripped[..end].contains(char::is_whitespace)
                    {
                        // The whitespace guard excludes this scanner's own
                        // source: its `.route(` and `"` literals otherwise
                        // self-match and extract bogus "patterns" spanning
                        // code, not an actual route string.
                        out.insert(stripped[..end].to_string());
                    }
                }
            }
        }
    }
}
