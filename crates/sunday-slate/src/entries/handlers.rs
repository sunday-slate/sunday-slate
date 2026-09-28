use std::collections::HashMap;
use std::convert::Infallible;
use std::time::Duration;

use askama::Template;
use axum::extract::{Path, State};
use axum::response::sse::{Event, KeepAlive};
use axum::response::{Html, Sse};
use futures_util::stream::{self, Stream};

use crate::auth::CurrentUser;
use crate::chrome::Chrome;
use crate::entries::EntryId;
use crate::entries::service::{self, EntryPage, EntryPreview, EntryView};
use crate::entries::store;
use crate::{AppError, AppState};

#[derive(Template)]
#[template(
    path = "entries/show.html",
    blocks = ["score_summary", "live_indicator", "score_regions"]
)]
struct ShowPage {
    page: EntryPage,
    chrome: Chrome,
    entry_id: i64,
    fragment: bool,
}

#[derive(Template)]
#[template(path = "entries/preview.html")]
struct PreviewPage {
    page: EntryPreview,
    chrome: Chrome,
}

pub async fn show(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<i64>,
) -> Result<Html<String>, AppError> {
    let view = service::page(&state, &user, EntryId(id)).await?;
    let contest_id = view.contest_id().0;
    match view {
        EntryView::Scored(page) => {
            let chrome = Chrome::contest("Lineup Scores", contest_id);
            Ok(Html(
                ShowPage {
                    page,
                    chrome,
                    entry_id: id,
                    fragment: false,
                }
                .render()?,
            ))
        }
        EntryView::Preview(page) => {
            let title = format!("{} Lineup", page.contest_name);
            let chrome = Chrome::contest(title, contest_id);
            Ok(Html(PreviewPage { page, chrome }.render()?))
        }
    }
}

pub async fn events(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<i64>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, AppError> {
    let entry_id = EntryId(id);
    let shutdown = state.live.shutdown_receiver();
    if *shutdown.borrow() {
        return Err(AppError::NotFound);
    }
    let entry = store::by_id(state.db.reader(), entry_id)
        .await?
        .ok_or(AppError::NotFound)?;
    if !state.live.enabled() {
        return Err(AppError::NotFound);
    }
    let revisions = state.live.subscribe(entry.contest.id);

    let view = service::page(&state, &user, entry_id).await?;
    let EntryView::Scored(mut page) = view else {
        return Err(AppError::NotFound);
    };
    if page.score.events_url.is_none() {
        return Err(AppError::NotFound);
    }
    let mut previous_scores = HashMap::new();
    service::update_score_directions(&mut page, &mut previous_scores);
    let initial = Event::default().data(render_fragment(page, entry_id.0)?);

    let stream = stream::unfold(
        (
            Some(initial),
            revisions,
            shutdown,
            state,
            user,
            entry_id,
            entry.league_id,
            false,
            previous_scores,
        ),
        |(
            initial,
            mut revisions,
            mut shutdown,
            state,
            user,
            entry_id,
            league_id,
            mut official_sent,
            mut previous_scores,
        )| async move {
            if let Some(event) = initial {
                if *shutdown.borrow() {
                    return None;
                }
                return Some((
                    Ok(event),
                    (
                        None,
                        revisions,
                        shutdown,
                        state,
                        user,
                        entry_id,
                        league_id,
                        official_sent,
                        previous_scores,
                    ),
                ));
            }
            loop {
                if *shutdown.borrow() {
                    return None;
                }
                if official_sent {
                    tokio::select! {
                        result = shutdown.changed() => {
                            if result.is_err() || *shutdown.borrow() {
                                return None;
                            }
                        }
                        result = revisions.changed() => {
                            if result.is_err() || *shutdown.borrow() {
                                return None;
                            }
                            let view = match service::page(&state, &user, entry_id).await {
                                Ok(view) => view,
                                Err(error) => {
                                    tracing::warn!(
                                        error = ?error,
                                        league_id,
                                        entry_id = entry_id.0,
                                        "closing official entry stream after access loss"
                                    );
                                    return None;
                                }
                            };
                            if !matches!(view, EntryView::Scored(_)) {
                                tracing::warn!(
                                    league_id,
                                    entry_id = entry_id.0,
                                    "closing official entry stream after access loss"
                                );
                                return None;
                            }
                        }
                    }
                    continue;
                }
                tokio::select! {
                    result = shutdown.changed() => {
                        if result.is_err() || *shutdown.borrow() {
                            return None;
                        }
                    }
                    result = revisions.changed() => {
                        if result.is_err() || *shutdown.borrow() {
                            return None;
                        }
                        let view = match service::page(&state, &user, entry_id).await {
                            Ok(view) => view,
                            Err(error) => {
                                tracing::warn!(
                                    error = ?error,
                                    league_id,
                                    entry_id = entry_id.0,
                                    "closing entry live stream after page lookup failure"
                                );
                                return None;
                            }
                        };
                        let EntryView::Scored(mut page) = view else {
                            return None;
                        };
                        let is_official = page.score.official;
                        if !is_official && page.score.events_url.is_none() {
                            tracing::warn!(
                                league_id,
                                entry_id = entry_id.0,
                                "closing entry live stream after eligibility loss"
                            );
                            return None;
                        }
                        service::update_score_directions(&mut page, &mut previous_scores);
                        let data = match render_fragment(page, entry_id.0) {
                            Ok(data) => data,
                            Err(error) => {
                                tracing::warn!(
                                    error = ?error,
                                    league_id,
                                    entry_id = entry_id.0,
                                    "closing entry live stream after render failure"
                                );
                                return None;
                            }
                        };
                        if *shutdown.borrow() {
                            return None;
                        }
                        official_sent = is_official;
                        return Some((
                            Ok(Event::default().data(data)),
                            (
                                None,
                                revisions,
                                shutdown,
                                state,
                                user,
                                entry_id,
                                league_id,
                                official_sent,
                                previous_scores,
                            ),
                        ));
                    }
                }
            }
        },
    );

    Ok(Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    ))
}

fn render_fragment(page: EntryPage, entry_id: i64) -> Result<String, askama::Error> {
    let contest_id = page.contest_id.0;
    let template = ShowPage {
        page,
        chrome: Chrome::contest("Lineup Scores", contest_id),
        entry_id,
        fragment: true,
    };
    Ok(format!(
        "{}<div hx-swap-oob=\"innerHTML:#entry-{entry_id}-score-summary\">{}</div>{}",
        template.as_live_indicator().render()?,
        template.as_score_summary().render()?,
        template.as_score_regions().render()?,
    ))
}
