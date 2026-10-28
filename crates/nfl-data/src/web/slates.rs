use askama::Template;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use time::OffsetDateTime;
use time::format_description::FormatItem;
use time::macros::format_description;

use crate::slates::{SlateId, all, by_id, games};
use crate::web::AdminState;
use crate::web::error::AdminError;
use nfl_model::{SeasonWeek, to_eastern};

#[derive(Template)]
#[template(path = "admin/slates/index.html", config = "src/web/askama.toml")]
pub(crate) struct IndexTemplate {
    pub(crate) slates: Vec<SlateRow>,
}

pub(crate) struct SlateRow {
    pub(crate) id: i64,
    pub(crate) name: String,
    pub(crate) game_count: usize,
    pub(crate) kickoff: String,
}

#[derive(Template)]
#[template(path = "admin/slates/show.html", config = "src/web/askama.toml")]
pub(crate) struct ShowTemplate {
    pub(crate) name: String,
    pub(crate) week_label: String,
    pub(crate) kickoff: String,
    pub(crate) games: Vec<GameRow>,
}

pub(crate) struct GameRow {
    pub(crate) matchup: String,
    pub(crate) kickoff: String,
}

/// `2025 · Week 7` or `2025 · Week 19 · Postseason`.
fn week_label(week: SeasonWeek) -> String {
    if week.season_type.is_reg() {
        format!("{} · Week {}", week.season, week.week)
    } else {
        format!("{} · Week {} · Postseason", week.season, week.week)
    }
}

/// Eastern kickoffs like `Oct 18`; `None` is `Kickoff unknown`.
fn kickoff_label(kickoff: Option<OffsetDateTime>) -> String {
    kickoff
        .map(|kickoff| {
            to_eastern(kickoff)
                .format(&KICKOFF_FORMAT)
                .unwrap_or_else(|_| kickoff.to_string())
        })
        .unwrap_or_else(|| "Kickoff unknown".into())
}

/// Game kickoffs like `Sun @ 1:00pm`; `None` is `Kickoff unknown`.
fn game_kickoff_label(kickoff: Option<OffsetDateTime>) -> String {
    kickoff
        .map(|kickoff| {
            to_eastern(kickoff)
                .format(&GAME_KICKOFF_FORMAT)
                .unwrap_or_else(|_| kickoff.to_string())
                .to_lowercase()
        })
        .unwrap_or_else(|| "Kickoff unknown".into())
}

const KICKOFF_FORMAT: &[FormatItem] = format_description!("[month repr:short] [day padding:none]");

const GAME_KICKOFF_FORMAT: &[FormatItem] = format_description!(
    "[weekday repr:short] @ [hour repr:12 padding:none]:[minute] [period]"
);

pub(super) async fn index(State(state): State<AdminState>) -> Result<Html<String>, AdminError> {
    let slates = all(&state.nfl).await?;

    let rows = slates
        .into_iter()
        .map(|(slate, count)| SlateRow {
            id: slate.id.0,
            name: slate.name,
            game_count: count,
            kickoff: kickoff_label(slate.kickoff),
        })
        .collect();

    Ok(Html(IndexTemplate { slates: rows }.render()?))
}

pub(super) async fn show(
    State(state): State<AdminState>,
    Path(id): Path<i64>,
) -> Result<Response, AdminError> {
    let id = SlateId(id);
    let Some(slate) = by_id(&state.nfl, id).await? else {
        return Ok(StatusCode::NOT_FOUND.into_response());
    };
    if slate.season_week.season != state.nfl.season() {
        return Ok(StatusCode::NOT_FOUND.into_response());
    }

    let games = games(&state.nfl, id).await?;
    let rows = games
        .into_iter()
        .map(|game| GameRow {
            matchup: format!("{} at {}", game.away_team, game.home_team),
            kickoff: game_kickoff_label(game.kickoff),
        })
        .collect();

    Ok(Html(
        ShowTemplate {
            name: slate.name,
            week_label: week_label(slate.season_week),
            kickoff: kickoff_label(slate.kickoff),
            games: rows,
        }
        .render()?,
    )
    .into_response())
}
