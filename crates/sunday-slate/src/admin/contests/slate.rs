//! The commissioner's slate editor: choose which games a contest covers, then
//! publish them. These two handlers are the only code that writes
//! `contest_games`.

use std::collections::HashMap;

use askama::Template;
use axum::extract::{Path, State};
use axum::response::{Html, IntoResponse, Response};
use axum_htmx::HxRequest;
use axum_messages::Messages;
use nfl_data::{Game, Season};

use crate::chrome::Chrome;
use crate::contests::rule::{self, Rule};
use crate::contests::service::{GameRow, game_rows, slate_locked};
use crate::contests::store as contest_store;
use crate::contests::{ContestId, published_slate};
use crate::web::redirect;
use crate::{AppError, AppState};

/// One box in the editor: a slate row plus whether it starts checked.
pub struct GameChoice {
    pub game: GameRow,
    pub checked: bool,
}

#[derive(Template)]
#[template(path = "admin/contests/slate.html")]
pub struct SlatePage {
    pub contest_id: i64,
    pub choices: Vec<GameChoice>,
    pub locked: bool,
    pub published: bool,
    pub chrome: Chrome,
}

/// The editor's chrome, back-linking to the contest list.
fn slate_chrome(contest_name: &str) -> Chrome {
    Chrome::focused(format!("{contest_name} slate"), "/admin/contests")
}

/// Which boxes start checked: the published slate when one exists, otherwise
/// the formula's proposal.
fn default_selection(
    published: &[Game],
    rule: &Rule,
    season: u16,
    season_games: &[Game],
) -> Vec<String> {
    if !published.is_empty() {
        return published.iter().map(|g| g.gsis_game_id.clone()).collect();
    }
    rule::resolve(rule, season, season_games)
        .into_iter()
        .map(|g| g.gsis_game_id)
        .collect()
}

/// The pool as boxes, in kickoff order — `game_rows` sorts and formats them.
fn choices(pool: &[Game], selected: &[String]) -> Vec<GameChoice> {
    game_rows(pool)
        .into_iter()
        .map(|game| GameChoice {
            checked: selected.contains(&game.id),
            game,
        })
        .collect()
}

/// Everything the editor needs, loaded once for both handlers.
struct Editing {
    contest: crate::contests::Contest,
    rule: Rule,
    season: u16,
    season_games: Vec<Game>,
    published: Vec<Game>,
}

async fn load(state: &AppState, id: ContestId) -> Result<Editing, AppError> {
    let contest = contest_store::by_id(state.db.reader(), id)
        .await?
        .ok_or(AppError::NotFound)?;
    let rule = contest.rule().ok_or(AppError::NotFound)?;
    let season = state.config.season;
    let season_games = state.nfl.games(Season(season)).await?;
    let published = published_slate(state.db.reader(), id, &season_games).await?;
    Ok(Editing {
        contest,
        rule,
        season,
        season_games,
        published,
    })
}

pub async fn editor(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Response, AppError> {
    let id = ContestId(id);
    let e = load(&state, id).await?;
    let selected = default_selection(&e.published, &e.rule, e.season, &e.season_games);
    let pool = rule::candidates(&e.rule, e.season, &e.season_games);
    let page = SlatePage {
        contest_id: id.0,
        choices: choices(&pool, &selected),
        locked: slate_locked(&e.published, state.now()),
        published: !e.published.is_empty(),
        chrome: slate_chrome(&e.contest.name),
    };
    Ok(Html(page.render()?).into_response())
}

/// The ids whose checkbox came back. Each box carries its own field name,
/// because Axum's form decoder cannot collect a repeated name into a list.
fn selected_ids(form: &HashMap<String, String>) -> Vec<String> {
    let mut ids: Vec<String> = form
        .keys()
        .filter_map(|k| k.strip_prefix("game_").map(str::to_string))
        .collect();
    ids.sort();
    ids
}

/// The two rules a submission must satisfy. The pool is derived on the server,
/// so the second rule catches only a tampered or stale form.
fn validate(submitted: &[String], pool: &[Game]) -> Result<(), &'static str> {
    if submitted.is_empty() {
        return Err("Select at least one game.");
    }
    if submitted
        .iter()
        .any(|id| !pool.iter().any(|g| &g.gsis_game_id == id))
    {
        return Err("That game is not in this contest's week. Reload the page and try again.");
    }
    Ok(())
}

pub async fn publish(
    HxRequest(is_htmx): HxRequest,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    messages: Messages,
    axum::Form(form): axum::Form<HashMap<String, String>>,
) -> Result<Response, AppError> {
    let id = ContestId(id);
    let e = load(&state, id).await?;
    let back = format!("/admin/contests/{}/slate", id.0);

    if slate_locked(&e.published, state.now()) {
        messages.error("These games have started. The slate can no longer change.");
        return Ok(redirect(is_htmx, back));
    }

    let ids = selected_ids(&form);
    let pool = rule::candidates(&e.rule, e.season, &e.season_games);
    if let Err(message) = validate(&ids, &pool) {
        messages.error(message);
        return Ok(redirect(is_htmx, back));
    }

    let count = ids.len();
    state
        .db
        .write_tx::<_, (), AppError>(async move |conn| {
            Ok(contest_store::set_games(conn, id, &ids).await?)
        })
        .await?;
    messages.success(format!("Published {count} games."));
    Ok(redirect(is_htmx, "/admin/contests"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use nfl_data::{Season, SeasonType, TeamAbbr as NflTeamAbbr, Week};
    use time::OffsetDateTime;
    use time::macros::datetime;

    fn g(id: &str, kickoff: Option<OffsetDateTime>) -> Game {
        Game {
            gsis_game_id: id.into(),
            season: Season(2025),
            week: Week(5),
            season_type: SeasonType::Reg,
            kickoff,
            away_team: NflTeamAbbr("BUF".into()),
            home_team: NflTeamAbbr("KC".into()),
            home_score: None,
            away_score: None,
        }
    }

    #[test]
    fn a_first_visit_defaults_to_the_formula() {
        let season_games = vec![
            g("thu", Some(datetime!(2025-10-03 00:15 UTC))),
            g("sun_1pm", Some(datetime!(2025-10-05 17:00 UTC))),
        ];
        // `resolve` keeps only the Sunday afternoon game.
        assert_eq!(
            default_selection(&[], &Rule::Main { week: 5 }, 2025, &season_games),
            vec!["sun_1pm".to_string()]
        );
    }

    #[test]
    fn a_return_visit_defaults_to_what_was_published() {
        let season_games = vec![
            g("thu", Some(datetime!(2025-10-03 00:15 UTC))),
            g("sun_1pm", Some(datetime!(2025-10-05 17:00 UTC))),
        ];
        let published = vec![g("thu", Some(datetime!(2025-10-03 00:15 UTC)))];
        assert_eq!(
            default_selection(&published, &Rule::Main { week: 5 }, 2025, &season_games),
            vec!["thu".to_string()]
        );
    }

    #[test]
    fn an_unpublished_contest_is_never_locked() {
        assert!(!slate_locked(&[], datetime!(2030-01-01 00:00 UTC)));
    }

    #[test]
    fn a_published_slate_locks_at_its_earliest_kickoff() {
        let published = vec![
            g("late", Some(datetime!(2025-10-05 20:25 UTC))),
            g("early", Some(datetime!(2025-10-05 17:00 UTC))),
        ];
        assert!(!slate_locked(&published, datetime!(2025-10-05 16:59 UTC)));
        assert!(slate_locked(&published, datetime!(2025-10-05 17:00 UTC)));
    }
    #[test]
    fn a_mixed_slate_locks_at_its_earliest_known_kickoff() {
        let published = vec![
            g("known", Some(datetime!(2025-10-05 17:00 UTC))),
            g("tbd", None),
        ];
        assert!(!slate_locked(&published, datetime!(2025-10-05 16:59 UTC)));
        assert!(slate_locked(&published, datetime!(2025-10-05 17:00 UTC)));
    }

    #[test]
    fn a_slate_with_no_known_kickoff_stays_editable() {
        assert!(!slate_locked(
            &[g("tbd", None)],
            datetime!(2030-01-01 00:00 UTC)
        ));
    }

    #[test]
    fn choices_check_only_the_selected_games() {
        let pool = vec![
            g("thu", Some(datetime!(2025-10-03 00:15 UTC))),
            g("sun_1pm", Some(datetime!(2025-10-05 17:00 UTC))),
        ];
        let rows = choices(&pool, &["sun_1pm".to_string()]);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].game.id, "thu", "kickoff order, not pool order");
        assert!(!rows[0].checked);
        assert_eq!(rows[1].game.matchup, "BUF @ KC");
        assert!(rows[1].checked);
        assert!(rows[1].game.kickoff.is_some());
    }

    #[test]
    fn selected_ids_reads_the_checked_boxes() {
        let form: HashMap<String, String> = [
            ("game_sun_1pm".to_string(), "on".to_string()),
            ("game_thu".to_string(), "on".to_string()),
        ]
        .into_iter()
        .collect();
        assert_eq!(
            selected_ids(&form),
            vec!["sun_1pm".to_string(), "thu".to_string()]
        );
    }

    /// A stub as `published_slate` builds one for an id the schedule lost:
    /// empty team names, `Week(0)`, no kickoff.
    #[test]
    fn an_empty_selection_is_rejected() {
        let pool = vec![g("thu", None)];
        assert_eq!(validate(&[], &pool), Err("Select at least one game."));
    }

    #[test]
    fn an_id_outside_the_pool_is_rejected() {
        let pool = vec![g("thu", None)];
        assert!(validate(&["elsewhere".to_string()], &pool).is_err());
    }

    #[test]
    fn a_selection_inside_the_pool_is_accepted() {
        let pool = vec![g("thu", None), g("sun_1pm", None)];
        assert_eq!(validate(&["thu".to_string()], &pool), Ok(()));
    }

    #[test]
    fn the_page_renders() {
        SlatePage {
            contest_id: 1,
            choices: vec![GameChoice {
                game: GameRow {
                    id: "sun_1pm".into(),
                    matchup: "BUF @ KC".into(),
                    kickoff: Some("Sun, Oct 5 · 1:00 PM".into()),
                },
                checked: true,
            }],
            locked: false,
            published: false,
            chrome: slate_chrome("Week 5"),
        }
        .render()
        .expect("editable page");

        SlatePage {
            contest_id: 1,
            choices: vec![],
            locked: true,
            published: true,
            chrome: slate_chrome("Week 5"),
        }
        .render()
        .expect("locked page");
    }
}
