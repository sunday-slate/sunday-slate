//! Task-local request context for layout-level template data.
//!
//! Askama can't reach request state, so flash messages and current user are
//! carried in a [`tokio::task_local!`] populated by [`request_ctx`] middleware
//! and read by layout partials via [`flashes`] and [`current_user`].
//! Outside a request scope, accessors return defaults — no task-local setup
//! needed for direct-render template unit tests.

use crate::AppState;
use crate::Db;
use crate::fantasy_teams::FantasyTeam;
use crate::flash::{self, Flash};
use crate::leagues::League;
use axum::extract::{FromRequestParts, Request, State};
use axum::http::Method;
use axum::middleware::Next;
use axum::response::Response;
use axum_login::AuthSession;
use axum_messages::Messages;

/// Minimal view of the signed-in user for templates.
/// Excludes `password_hash` so templates can't read sensitive data via [`current_user`].
#[derive(Clone)]
pub struct CurrentUser {
    pub is_commissioner: bool,
    pub is_admin: bool,
}

/// One entry in the overflow menu's league switcher.
#[derive(Clone)]
pub struct SwitcherLeague {
    pub id: i64,
    pub name: String,
    pub active: bool,
}

/// Request-scoped data shared with layout templates.
#[derive(Default)]
struct RequestCtx {
    messages: Option<Messages>,
    user: Option<CurrentUser>,
    /// The Current Contest id — the Current tab's existence and target.
    current_contest: Option<i64>,
    league_name: Option<String>,
    team: Option<i64>,
    switcher: Vec<SwitcherLeague>,
    admin_banner: Option<crate::admin::banner::AdminBanner>,
    /// Set when an entity link silently switched the active league this
    /// request — `flashes()` turns it into a "Now viewing …" toast.
    league_switched: Option<String>,
}

tokio::task_local! {
    static REQUEST_CTX: RequestCtx;
}

/// Drain pending flash messages for this request, plus the implicit
/// league-switch toast when an entity link changed the active league.
/// Returns empty outside a request scope.
pub fn flashes() -> Vec<Flash> {
    REQUEST_CTX
        .try_with(|c| (c.messages.clone(), c.league_switched.clone()))
        .map(|(messages, switched)| {
            let mut out = messages.map(flash::drain).unwrap_or_default();
            if let Some(name) = switched {
                out.push(Flash::info(format!("Now viewing {name}")));
            }
            out
        })
        .unwrap_or_default()
}

/// The signed-in user, or None when unauthenticated or outside a request scope.
pub fn current_user() -> Option<CurrentUser> {
    REQUEST_CTX.try_with(|c| c.user.clone()).ok().flatten()
}

/// The Current Contest id — `None` off-season (hides the Current tab), on
/// non-GET/htmx requests, on the root redirect, and outside a request scope.
pub fn current_contest() -> Option<i64> {
    REQUEST_CTX.try_with(|c| c.current_contest).ok().flatten()
}

/// The current league's name — the `<title>` suffix.
/// None when signed out, pre-league, or outside a request scope.
pub fn league_name() -> Option<String> {
    REQUEST_CTX
        .try_with(|c| c.league_name.clone())
        .ok()
        .flatten()
}

/// The signed-in user's team — the tab bar's My Team link.
pub fn current_team() -> Option<i64> {
    REQUEST_CTX.try_with(|c| c.team).ok().flatten()
}

/// The switcher entries: the user's team leagues, lowest id first. Empty for
/// users with fewer than two leagues (no switcher shown) and outside a
/// request scope.
pub fn league_switcher() -> Vec<SwitcherLeague> {
    REQUEST_CTX
        .try_with(|c| c.switcher.clone())
        .unwrap_or_default()
}

/// The admin's pending-setup banner. `None` for members, ready contests,
/// admin pages (the banner links into `/admin`, so it would be noise
/// there), requests that render no shell, and outside a request scope.
pub fn admin_banner() -> Option<crate::admin::banner::AdminBanner> {
    REQUEST_CTX
        .try_with(|c| c.admin_banner.clone())
        .ok()
        .flatten()
}

/// Build the request context and run the rest of the stack inside the
/// task-local scope. Must be mounted inside `MessagesManagerLayer` and the
/// auth layer so both extensions are populated.
pub async fn request_ctx(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let (mut parts, body) = req.into_parts();
    let messages = Messages::from_request_parts(&mut parts, &()).await.ok();
    let signed_in = match parts.extensions.get::<AuthSession<Db>>().cloned() {
        Some(auth) => auth.user().await,
        None => None,
    };
    let team_info = parts
        .extensions
        .get::<Option<FantasyTeam>>()
        .and_then(|team| team.as_ref().map(|t| (t.id, t.is_commissioner)));
    let ctx = match signed_in {
        Some(u) => {
            let (league_id, league_name) = parts
                .extensions
                .get::<Option<League>>()
                .and_then(|league| league.as_ref().map(|l| (l.id, l.name.clone())))
                .unzip();
            let user = CurrentUser {
                is_commissioner: team_info.as_ref().is_some_and(|team| team.1),
                is_admin: u.is_admin,
            };
            let team = team_info.map(|(id, _)| id);
            let switcher = match crate::leagues::store::for_user(state.db.reader(), u.id).await {
                Ok(all) if all.len() >= 2 => all
                    .into_iter()
                    .map(|l| SwitcherLeague {
                        active: Some(l.id) == league_id,
                        id: l.id,
                        name: l.name,
                    })
                    .collect(),
                _ => Vec::new(),
            };
            let full_page_get =
                parts.method == Method::GET && !parts.headers.contains_key("HX-Request");
            // The banner is shell chrome, so only a full page render can
            // show it: POSTs and their redirects render no shell, htmx
            // requests swap a fragment inside one that already rendered,
            // and admin pages are where the banner sends the admin. Every
            // other request skips the load.
            let admin_banner = if u.is_admin
                && full_page_get
                && parts.uri.path() != "/"
                && !crate::admin::is_admin_path(parts.uri.path())
            {
                pending_banner(&state).await
            } else {
                None
            };
            // The tab bar renders only on full-page GETs; `/` redirects and
            // resolves its contest in the handler. Best-effort: an error
            // hides the tab.
            let current_contest = if full_page_get && parts.uri.path() != "/" {
                crate::home::service::current_contest_id(&state)
                    .await
                    .ok()
                    .flatten()
                    .map(|id| id.0)
            } else {
                None
            };
            let league_switched = parts
                .extensions
                .get::<crate::leagues::LeagueSwitched>()
                .cloned()
                .map(|s| s.0);
            RequestCtx {
                messages,
                user: Some(user),
                current_contest,
                league_name,
                team,
                switcher,
                admin_banner,
                league_switched,
            }
        }
        None => RequestCtx {
            messages,
            ..Default::default()
        },
    };
    let req = Request::from_parts(parts, body);
    REQUEST_CTX.scope(ctx, next.run(req)).await
}

/// The admin's pending-setup banner for a shell-rendering request outside
/// `/admin`. Best-effort — a failed load reads as `None`, which only hides
/// the reminder.
async fn pending_banner(state: &AppState) -> Option<crate::admin::banner::AdminBanner> {
    let season = nfl_data::Season(state.config.season);
    let slates =
        crate::contests::service::SeasonSlates::load(state.db.reader(), &state.nfl, season)
            .await
            .ok()?;
    let contests = crate::contests::store::all(state.db.reader()).await.ok()?;
    crate::admin::banner::pending(state, &slates, &contests, state.now_eastern())
        .await
        .ok()
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Accessors must return defaults outside a request scope so
    /// direct-render template unit tests work without the task-local.
    #[test]
    fn accessors_are_default_safe_outside_scope() {
        assert!(flashes().is_empty());
        assert!(current_user().is_none());
        assert!(current_contest().is_none());
        assert!(league_name().is_none());
        assert!(current_team().is_none());
        assert!(league_switcher().is_empty());
        assert!(admin_banner().is_none());
    }
}
