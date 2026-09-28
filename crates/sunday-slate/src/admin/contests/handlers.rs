use askama::Template;
use axum::Router;
use axum::extract::State;
use axum::response::{Html, IntoResponse};
use axum::routing::get;
use axum_htmx::HxRequest;
use serde::Deserialize;
use time::OffsetDateTime;

use crate::admin::contests::slate;
use crate::admin::contests::store::{OPTIONAL_CONTESTS, set_up};
use crate::chrome::Chrome;
use crate::contests::published_slate;
use crate::contests::rule::Rule;
use crate::contests::service::{format_day, game_rows};
use crate::contests::store as contest_store;
use crate::web::redirect;
use crate::{AppError, AppState};
use nfl_data::Season;

/// Routes for the admin contest-setup UI. Mounted under the admin router, which
/// applies the `AdminUser` gate for the whole subtree.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(index))
        .route("/setup", get(setup_form).post(setup_submit))
        .route("/{id}/slate", get(slate::editor).post(slate::publish))
}

/// The contest-list chrome for the contest-setup subtree.
fn list_chrome() -> Chrome {
    Chrome::focused("Contests", "/admin")
}

/// The chrome for the one-time setup form, back-linking to the list.
fn setup_chrome() -> Chrome {
    Chrome::focused("Set up contests", "/admin/contests")
}

struct ContestRow {
    id: i64,
    name: String,
    group: &'static str,
    /// Formatted start date (earliest kickoff), or `None` for an unset slate.
    start: Option<String>,
    matchups: Vec<String>,
}

fn group_of(rule: &Rule) -> &'static str {
    match rule {
        Rule::Main { .. } => "Regular Season",
        Rule::Holiday(_) => "Holidays",
        Rule::Playoff { .. } => "Playoffs",
    }
}

/// The contest's start date for display, e.g. "Sun, Sep 7, 2025", formatted in
/// whatever offset `dt` carries (callers pass Eastern). The season list is the
/// only place a year is shown, so it is the day format plus the year.
fn format_start(dt: OffsetDateTime) -> String {
    format!("{}, {}", format_day(dt), dt.year())
}

/// Calendar order for the contest list: earliest kickoff first, naturally
/// interleaving holiday slates among the weeks. Contests with no resolved
/// kickoff (empty slate) sort last; a stable sort keeps their incoming order.
fn calendar_cmp(a: Option<OffsetDateTime>, b: Option<OffsetDateTime>) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (a, b) {
        (Some(x), Some(y)) => x.cmp(&y),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

#[derive(Template)]
#[template(path = "admin/contests/index.html")]
struct IndexPage {
    rows: Vec<ContestRow>,
    chrome: Chrome,
}

pub async fn index(State(state): State<AppState>) -> Result<impl IntoResponse, AppError> {
    let season = state.config.season;
    let games = state.nfl.games(Season(season)).await?;

    let mut rows = Vec::new();
    for contest in contest_store::all(state.db.reader()).await? {
        let Some(rule) = contest.rule() else { continue };
        let slate = published_slate(state.db.reader(), contest.id, &games).await?;
        // Earliest kickoff, in Eastern, is the contest's start (and its sort key).
        let earliest = slate.iter().filter_map(|g| g.kickoff_eastern()).min();
        // `game_rows` formats and orders the slate the same way the contest
        // and editor pages do.
        let matchups: Vec<String> = game_rows(&slate).into_iter().map(|r| r.matchup).collect();
        rows.push((
            earliest,
            ContestRow {
                id: contest.id.0,
                name: contest.name,
                group: group_of(&rule),
                start: earliest.map(format_start),
                matchups,
            },
        ));
    }
    // Show contests in calendar order (earliest kickoff first) rather than the
    // insertion order `contest_store::all` returns, which appends the optionals
    // after the 18 weeks instead of interleaving them.
    rows.sort_by(|a, b| calendar_cmp(a.0, b.0));
    let rows: Vec<ContestRow> = rows.into_iter().map(|(_, row)| row).collect();
    Ok(Html(
        IndexPage {
            rows,
            chrome: list_chrome(),
        }
        .render()?,
    )
    .into_response())
}

struct OptionRow {
    field: String,
    name: String,
    checked: bool,
}

#[derive(Template)]
#[template(path = "admin/contests/setup.html")]
struct SetupPage {
    options: Vec<OptionRow>,
    chrome: Chrome,
}

pub async fn setup_form(
    HxRequest(is_htmx): HxRequest,
    State(state): State<AppState>,
) -> Result<impl IntoResponse, AppError> {
    // Setup runs once; after that the list is fixed and there is nothing to edit.
    if !contest_store::all(state.db.reader()).await?.is_empty() {
        return Ok(redirect(is_htmx, "/admin/contests"));
    }
    let options = OPTIONAL_CONTESTS
        .iter()
        .map(|(field, name)| OptionRow {
            field: field.to_string(),
            name: name.to_string(),
            // Nothing is configured yet, so every optional starts on.
            checked: true,
        })
        .collect();
    Ok(Html(
        SetupPage {
            options,
            chrome: setup_chrome(),
        }
        .render()?,
    )
    .into_response())
}

#[derive(Deserialize, Default)]
pub struct SetupForm {
    #[serde(default)]
    thanksgiving: Option<String>,
    #[serde(default)]
    christmas: Option<String>,
    #[serde(default)]
    wild_card: Option<String>,
    #[serde(default)]
    divisional: Option<String>,
    #[serde(default)]
    championship: Option<String>,
}

impl SetupForm {
    fn optional_on(&self) -> Vec<String> {
        let mut on = Vec::new();
        for (field, name) in OPTIONAL_CONTESTS {
            let checked = match field {
                "thanksgiving" => self.thanksgiving.is_some(),
                "christmas" => self.christmas.is_some(),
                "wild_card" => self.wild_card.is_some(),
                "divisional" => self.divisional.is_some(),
                "championship" => self.championship.is_some(),
                _ => false,
            };
            if checked {
                on.push(name.to_string());
            }
        }
        on
    }
}

pub async fn setup_submit(
    HxRequest(is_htmx): HxRequest,
    State(state): State<AppState>,
    axum::Form(form): axum::Form<SetupForm>,
) -> Result<impl IntoResponse, AppError> {
    let on = form.optional_on();
    // `set_up` ignores a resubmission once the season exists, so a stale tab can
    // neither add a contest nor remove one (which would cascade its entries).
    // The check runs inside the write transaction, so concurrent posts serialize.
    state
        .db
        .write_tx::<_, bool, AppError>(async move |conn| Ok(set_up(conn, &on).await?))
        .await?;
    Ok(redirect(is_htmx, "/admin/contests"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::UtcOffset;
    use time::macros::datetime;

    #[test]
    fn start_date_shows_weekday_month_day_year_in_its_offset() {
        // 2025-09-07 17:00Z is Sunday 1:00 PM Eastern (EDT, -4).
        let et = datetime!(2025-09-07 17:00 UTC).to_offset(UtcOffset::from_hms(-4, 0, 0).unwrap());
        assert_eq!(format_start(et), "Sun, Sep 7, 2025");
    }

    #[test]
    fn contests_sort_by_earliest_kickoff_interleaving_holidays() {
        // Inserted in id/creation order (weeks, then optionals appended) — the
        // pre-fix order. Calendar order must interleave the holidays and put the
        // empty-slate contest (no kickoff) last.
        let mut rows = [
            (Some(datetime!(2025-09-07 17:00 UTC)), "Week 1"),
            (Some(datetime!(2025-11-30 18:00 UTC)), "Week 13"),
            (Some(datetime!(2026-01-04 18:00 UTC)), "Week 18"),
            (None, "Empty"),
            (Some(datetime!(2025-12-25 18:00 UTC)), "Christmas"),
            (Some(datetime!(2025-11-27 18:00 UTC)), "Thanksgiving"),
            (Some(datetime!(2026-01-10 21:30 UTC)), "Wild Card"),
        ];
        rows.sort_by(|a, b| calendar_cmp(a.0, b.0));
        let order: Vec<&str> = rows.iter().map(|(_, n)| *n).collect();
        assert_eq!(
            order,
            vec![
                "Week 1",
                "Thanksgiving",
                "Week 13",
                "Christmas",
                "Week 18",
                "Wild Card",
                "Empty",
            ]
        );
    }

    #[test]
    fn pages_render() {
        IndexPage {
            rows: vec![],
            chrome: list_chrome(),
        }
        .render()
        .expect("index");
        IndexPage {
            rows: vec![ContestRow {
                id: 1,
                name: "Week 1".into(),
                group: "Regular Season",
                start: Some("Sun, Sep 7, 2025".into()),
                matchups: vec!["SF@LA".into()],
            }],
            chrome: list_chrome(),
        }
        .render()
        .expect("index with row");
        SetupPage {
            options: vec![OptionRow {
                field: "thanksgiving".into(),
                name: "Thanksgiving".into(),
                checked: true,
            }],
            chrome: setup_chrome(),
        }
        .render()
        .expect("setup");
    }
}
