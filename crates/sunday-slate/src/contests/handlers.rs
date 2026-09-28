use std::convert::Infallible;
use std::time::Duration;

use askama::Template;
use axum::extract::{Path, Query, State};
use axum::response::sse::{Event, KeepAlive};
use axum::response::{Html, Sse};
use futures_util::stream::{self, Stream};
use nfl_data::{LiveGamePhase, Season};
use serde::Deserialize;

use crate::auth::CurrentUser;
use crate::chrome::{Chrome, Tab};
use crate::contests::ContestId;
use crate::contests::service::{
    self, ContestRow, EntryRow, GameRow, SeasonSlates, Upcoming, WaitingRow, WinnerRow,
};
use crate::contests::store;
use crate::draft_entry::store as draft_entry_store;
use crate::entries::store as entries_store;
use crate::fantasy_teams::{self, FantasyTeamId, MaybeTeam};
use crate::leagues::CurrentLeague;
use crate::live::ContestScores;
use crate::standings::service as standings_service;
use crate::{AppError, AppState};

#[derive(Template)]
#[template(path = "contests/index.html")]
struct ContestListPage {
    chrome: Chrome,
    /// Upcoming and live contests, rendered as their own list above the
    /// finished ones (they sort first, newest date first).
    current: Vec<ContestRow>,
    finished: Vec<ContestRow>,
}

pub async fn index(
    State(state): State<AppState>,
    CurrentLeague(league): CurrentLeague,
) -> Result<Html<String>, AppError> {
    let season = Season(state.config.season);
    let schedule = SeasonSlates::load(state.db.reader(), &state.nfl, season).await?;
    let now = state.now_eastern();

    let contests = schedule.slate_contests(&store::all(state.db.reader()).await?);
    let contest_weeks = schedule.contest_weeks();
    let teams = standings_service::scored_teams(&state, league.id, &contest_weeks).await?;

    let mut rows = service::contest_list(&contests, &teams, now);

    for row in &mut rows {
        match crate::live::load_contest_scores(&state, row.id).await? {
            ContestScores::Official(stats) => {
                row.state = service::ContestState::Finished;
                let entries =
                    entries_store::by_contest(state.db.reader(), row.id, league.id).await?;
                row.winners =
                    service::rank_contest_entries(service::entrants(&entries, Some(&stats)))
                        .into_iter()
                        .filter(|entry| entry.winner)
                        .filter_map(|entry| {
                            entry.points.map(|points| WinnerRow {
                                name: entry.name,
                                owner_name: entry.owner_name,
                                points,
                            })
                        })
                        .collect();
                row.pending_official = false;
            }
            ContestScores::Provisional(snapshot) => {
                row.winners.clear();
                row.pending_official = snapshot
                    .game_states
                    .values()
                    .all(|game| game.phase == LiveGamePhase::Final);
                if row.pending_official {
                    row.state = service::ContestState::Finished;
                } else {
                    row.state = service::ContestState::Live;
                }
            }
            ContestScores::Upcoming => {}
            ContestScores::Unavailable => {
                if row.is_finished() {
                    row.winners.clear();
                    row.pending_official = true;
                }
            }
        }
    }

    let (current, finished): (Vec<ContestRow>, Vec<ContestRow>) =
        rows.into_iter().partition(|r| !r.is_finished());
    Ok(Html(
        ContestListPage {
            chrome: Chrome::tabbed("Contests", Tab::Contests),
            current,
            finished,
        }
        .render()?,
    ))
}

#[derive(Template)]
#[template(
    path = "contests/show.html",
    blocks = ["live_indicator", "score_regions"]
)]
struct ContestPage {
    chrome: Chrome,
    /// The `/contests/{id}/draft-entry` link target for the viewer's own
    /// waiting row.
    contest_id: i64,
    rows: Vec<EntryRow>,
    /// League teams still to enter, pre-kickoff only.
    waiting: Vec<WaitingRow>,
    games: Vec<GameRow>,
    /// The owner's lineup action while the slate is open: enter, resume, or
    /// edit.
    draft_action: Option<DraftAction>,
    /// Pre-kickoff header: when entries lock and how many teams are in.
    upcoming: Option<Upcoming>,
    score: service::ContestScoreMeta,
    fragment: bool,
}

/// A contest-page action linking into the lineup editor.
struct DraftAction {
    href: String,
    kind: LineupAction,
}

/// The owner's one lineup action while the slate is open.
enum LineupAction {
    Enter,
    Resume,
}

impl LineupAction {
    /// The button's copy.
    fn label(&self) -> &'static str {
        match self {
            LineupAction::Enter => "Enter lineup",
            LineupAction::Resume => "Resume lineup",
        }
    }
}

pub async fn show(
    State(state): State<AppState>,
    CurrentLeague(league): CurrentLeague,
    MaybeTeam(team): MaybeTeam,
    Path(id): Path<String>,
) -> Result<Html<String>, AppError> {
    let id: i64 = id.parse().map_err(|_| AppError::NotFound)?;
    let viewer = team.map(|t| FantasyTeamId(t.id));
    let details = service::details_for_league(&state, league.id, viewer, ContestId(id))
        .await?
        .ok_or(AppError::NotFound)?;

    // While the slate is open and the viewer hasn't entered, offer the one
    // lineup action: enter, or resume a partial draft. Once entered, the
    // viewer's highlighted roll-call row is the affordance — no CTA.
    let draft_action = match (viewer, details.entries_open) {
        (Some(team), true) if details.my_entry.is_none() => {
            let resume = draft_entry_store::by_keys(state.db.reader(), ContestId(id), team)
                .await?
                .is_some_and(|d| d.has_picks());
            let kind = if resume {
                LineupAction::Resume
            } else {
                LineupAction::Enter
            };
            Some(DraftAction {
                href: format!("/contests/{id}/draft-entry"),
                kind,
            })
        }
        _ => None,
    };

    Ok(Html(
        ContestPage {
            score: details.score,
            fragment: false,
            chrome: Chrome::contest(details.name, id),
            contest_id: id,
            rows: details.rows,
            waiting: details.waiting,
            games: details.games,
            draft_action,
            upcoming: details.upcoming,
        }
        .render()?,
    ))
}

#[derive(Deserialize)]
pub struct ContestEventsQuery {
    pub league_id: i64,
}

pub async fn events(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Query(query): Query<ContestEventsQuery>,
    Path(id): Path<String>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, AppError> {
    let shutdown = state.live.shutdown_receiver();
    if *shutdown.borrow() {
        return Err(AppError::NotFound);
    }
    let contest_id = ContestId(id.parse().map_err(|_| AppError::NotFound)?);
    let team = fantasy_teams::store::team_for_user(state.db.reader(), query.league_id, user.id)
        .await?
        .ok_or(AppError::NotFound)?;
    if !state.live.enabled() {
        return Err(AppError::NotFound);
    }
    let revisions = state.live.subscribe(contest_id);

    let details = service::details_for_league(
        &state,
        query.league_id,
        Some(FantasyTeamId(team.id)),
        contest_id,
    )
    .await?
    .ok_or(AppError::NotFound)?;
    if details.score.events_url.is_none() {
        return Err(AppError::NotFound);
    }
    let initial = Event::default().data(render_fragment(details, contest_id.0)?);

    let stream = stream::unfold(
        (
            Some(initial),
            revisions,
            shutdown,
            state,
            user,
            query.league_id,
            contest_id,
            false,
        ),
        |(
            initial,
            mut revisions,
            mut shutdown,
            state,
            user,
            league_id,
            contest_id,
            mut official_sent,
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
                        league_id,
                        contest_id,
                        official_sent,
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
                            let Some(team) = fantasy_teams::store::team_for_user(
                                state.db.reader(),
                                league_id,
                                user.id,
                            )
                            .await
                            .ok()
                            .flatten() else {
                                tracing::warn!(
                                    league_id,
                                    contest_id = contest_id.0,
                                    user_id = user.id,
                                    "closing official contest stream after membership loss"
                                );
                                return None;
                            };
                            if service::details_for_league(
                                &state,
                                league_id,
                                Some(FantasyTeamId(team.id)),
                                contest_id,
                            )
                            .await
                            .ok()
                            .flatten()
                            .is_none()
                            {
                                tracing::warn!(
                                    league_id,
                                    contest_id = contest_id.0,
                                    "closing official contest stream after access loss"
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
                        let Some(team) = fantasy_teams::store::team_for_user(
                            state.db.reader(),
                            league_id,
                            user.id,
                        )
                        .await
                        .ok()
                        .flatten() else {
                            tracing::warn!(
                                league_id,
                                contest_id = contest_id.0,
                                user_id = user.id,
                                "closing contest live stream after membership loss"
                            );
                            return None;
                        };
                        let Some(details) = service::details_for_league(
                            &state,
                            league_id,
                            Some(FantasyTeamId(team.id)),
                            contest_id,
                        )
                        .await
                        .ok()
                        .flatten() else {
                            tracing::warn!(
                                league_id,
                                contest_id = contest_id.0,
                                "closing contest live stream after contest lookup failure"
                            );
                            return None;
                        };
                        let is_official = details.score.official;
                        if !is_official && details.score.events_url.is_none() {
                            tracing::warn!(
                                league_id,
                                contest_id = contest_id.0,
                                "closing contest live stream after eligibility loss"
                            );
                            return None;
                        }
                        let data = match render_fragment(details, contest_id.0) {
                            Ok(data) => data,
                            Err(error) => {
                                tracing::warn!(
                                    error = ?error,
                                    league_id,
                                    contest_id = contest_id.0,
                                    "closing contest live stream after render failure"
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
                                league_id,
                                contest_id,
                                official_sent,
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

fn render_fragment(
    details: service::ContestDetails,
    contest_id: i64,
) -> Result<String, askama::Error> {
    let title = details.name.clone();
    let template = ContestPage {
        chrome: Chrome::contest(title, contest_id),
        contest_id,
        rows: details.rows,
        waiting: details.waiting,
        games: details.games,
        draft_action: None,
        upcoming: details.upcoming,
        score: details.score,
        fragment: true,
    };
    Ok(format!(
        "{}{}",
        template.as_live_indicator().render()?,
        template.as_score_regions().render()?,
    ))
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::entries::EntryId;
    use crate::tests::utils::list_item_containing;

    fn row(name: &str, rank: Option<u32>, points: Option<f64>, winner: bool) -> EntryRow {
        EntryRow {
            id: EntryId(1),

            rank,
            name: name.to_string(),
            owner_name: "Mike".to_string(),
            logo: None,
            points,
            coverage: crate::live::Coverage::Unavailable,
            winner,
            mine: false,
            minutes_remaining: None,
        }
    }

    fn game(matchup: &str, kickoff: Option<&str>) -> GameRow {
        GameRow {
            id: matchup.to_string(),
            matchup: matchup.to_string(),
            kickoff: kickoff.map(str::to_string),
        }
    }
    fn page(rows: Vec<EntryRow>, games: Vec<GameRow>) -> ContestPage {
        ContestPage {
            chrome: Chrome::tabbed("Wild Card", Tab::Contests),
            contest_id: 1,
            rows,
            waiting: Vec::new(),
            games,
            draft_action: None,
            upcoming: None,
            score: Default::default(),
            fragment: false,
        }
    }

    fn upcoming() -> Upcoming {
        Upcoming {
            kickoff: "Sun, Nov 23 · 1:00 PM ET".to_string(),
            countdown: "in 1 hour".to_string(),
        }
    }

    fn waiting(name: &str, mine: bool) -> WaitingRow {
        WaitingRow {
            name: name.to_string(),
            owner_name: "Dana".to_string(),
            logo: None,
            mine,
        }
    }

    #[test]
    fn renders_live_indicator() {
        let html = ContestPage {
            score: service::ContestScoreMeta {
                coverage: crate::live::Coverage::Complete,
                events_url: Some("/contests/1/events?league_id=1".into()),
                active: true,
                delayed: false,
                official: false,
            },
            ..page(vec![], vec![])
        }
        .render()
        .expect("render");

        assert!(
            html.contains(r#"id="contest-1-live-indicator""#),
            "navbar indicator renders: {html}"
        );
        assert!(
            html.contains(r#"class="size-2 rounded-full bg-success hidden""#),
            "indicator is green and hidden before connection: {html}"
        );
        assert!(
            html.contains(r#"aria-label="Live score updates" title="Live score updates""#),
            "green indicator labels live updates: {html}"
        );
        assert!(html.contains("hx-on::after:sse:connection"), "{html}");
        assert!(html.contains("hx-on::sse:error"), "{html}");
        assert!(html.contains("hx-on::sse:close"), "{html}");
        assert!(!html.contains("Contest scores"), "{html}");
        assert!(!html.contains("score-summary"), "{html}");
        assert!(!html.contains(">LIVE<"), "{html}");
    }

    #[test]
    fn renders_indicator_fragment_labels() {
        let partial = ContestPage {
            score: service::ContestScoreMeta {
                coverage: crate::live::Coverage::Partial,
                events_url: Some("/contests/1/events?league_id=1".into()),
                active: true,
                delayed: false,
                official: false,
            },
            fragment: true,
            ..page(vec![], vec![])
        }
        .as_live_indicator()
        .render()
        .expect("render partial fragment");
        assert!(
            partial.contains(r#"class="size-2 rounded-full bg-warning" role="img""#),
            "partial fragment is yellow and visible: {partial}"
        );
        assert!(
            partial.contains(
                r#"aria-label="Live score data incomplete" title="Live score data incomplete""#
            ),
            "partial fragment labels incomplete data: {partial}"
        );

        let pending = ContestPage {
            score: service::ContestScoreMeta {
                coverage: crate::live::Coverage::Complete,
                events_url: Some("/contests/1/events?league_id=1".into()),
                active: false,
                delayed: false,
                official: false,
            },
            fragment: true,
            ..page(vec![], vec![])
        }
        .as_live_indicator()
        .render()
        .expect("render pending fragment");
        assert!(
            pending.contains(r#"class="size-2 rounded-full bg-warning" role="img""#),
            "pending fragment is yellow and visible: {pending}"
        );
        assert!(
            pending.contains(
                r#"aria-label="Live score updates pending" title="Live score updates pending""#
            ),
            "pending fragment labels provisional data: {pending}"
        );

        let delayed = ContestPage {
            score: service::ContestScoreMeta {
                coverage: crate::live::Coverage::Partial,
                events_url: Some("/contests/1/events?league_id=1".into()),
                active: true,
                delayed: true,
                official: false,
            },
            fragment: true,
            ..page(vec![], vec![])
        }
        .as_live_indicator()
        .render()
        .expect("render delayed fragment");
        assert!(
            delayed.contains(r#"class="size-2 rounded-full bg-warning" role="img""#),
            "delayed fragment is yellow and visible: {delayed}"
        );
        assert!(
            delayed.contains(
                r#"aria-label="Live score updates delayed" title="Live score updates delayed""#
            ),
            "delayed fragment labels stale data: {delayed}"
        );
        assert!(
            delayed.contains(r#"hx-swap-oob="outerHTML:#contest-1-live-indicator""#),
            "delayed fragment targets the navbar indicator: {delayed}"
        );

        let official = ContestPage {
            score: service::ContestScoreMeta {
                coverage: crate::live::Coverage::Complete,
                events_url: None,
                active: false,
                delayed: false,
                official: true,
            },
            fragment: true,
            ..page(vec![], vec![])
        }
        .as_live_indicator()
        .render()
        .expect("render official fragment");
        assert!(
            official.contains(r#"class="size-2 rounded-full bg-warning hidden""#),
            "official fragment hides the indicator: {official}"
        );
        assert!(
            official.contains(
                r#"aria-label="Official stats finalized" title="Official stats finalized""#
            ),
            "official fragment labels finalized stats: {official}"
        );
        assert!(
            official.contains(r#"hx-swap-oob="outerHTML:#contest-1-live-indicator""#),
            "official fragment replaces the stale indicator: {official}"
        );
    }

    #[test]
    fn entry_meters_render_only_when_minutes_present() {
        let mut live_row = row("Gridiron Giants", Some(1), Some(23.0), false);
        live_row.minutes_remaining = Some(342.0);
        let html = ContestPage {
            score: service::ContestScoreMeta {
                coverage: crate::live::Coverage::Complete,
                events_url: Some("/contests/1/events?league_id=1".into()),
                active: true,
                delayed: false,
                official: false,
            },
            fragment: true,
            rows: vec![live_row],
            ..page(vec![], vec![])
        }
        .render()
        .expect("render");

        assert!(
            html.contains("minutes-meter minutes-meter--ok"),
            "live row renders a meter: {html}"
        );
        assert!(
            html.contains(r#"style="width: 63%;""#),
            "342 of 540 minutes fills 63%: {html}"
        );
        assert!(html.contains("342m"), "meter labels its minutes: {html}");

        let official = ContestPage {
            score: service::ContestScoreMeta {
                coverage: crate::live::Coverage::Complete,
                events_url: None,
                active: false,
                delayed: false,
                official: true,
            },
            fragment: true,
            rows: vec![row("Gridiron Giants", Some(1), Some(23.0), false)],
            ..page(vec![], vec![])
        }
        .render()
        .expect("render official");

        assert!(
            !official.contains("minutes-meter"),
            "official rows carry no meter: {official}"
        );
        assert!(!official.contains("342m"), "{official}");
    }

    #[test]
    fn minutes_meter_is_identical_in_page_and_fragment() {
        let with_meter = |fragment: bool| {
            let mut live_row = row("Gridiron Giants", Some(1), Some(23.0), false);
            live_row.minutes_remaining = Some(342.0);
            ContestPage {
                score: service::ContestScoreMeta {
                    coverage: crate::live::Coverage::Complete,
                    events_url: Some("/contests/1/events?league_id=1".into()),
                    active: true,
                    delayed: false,
                    official: false,
                },
                fragment,
                rows: vec![live_row],
                ..page(vec![], vec![])
            }
            .render()
            .expect("render")
        };

        let page_html = with_meter(false);
        let fragment_html = with_meter(true);
        for html in [&page_html, &fragment_html] {
            assert!(html.contains("minutes-meter minutes-meter--ok"), "{html}");
            assert!(html.contains(r#"style="width: 63%;""#), "{html}");
            assert!(html.contains("342m"), "{html}");
        }
    }

    #[test]
    fn renders_ranked_rows_with_winner_trophy() {
        let html = page(
            vec![
                row("Turf Titans", Some(1), Some(23.0), true),
                row("Gridiron Giants", Some(2), Some(18.0), false),
            ],
            vec![],
        )
        .render()
        .expect("render");
        assert!(html.contains("Wild Card"));
        assert!(html.contains("Turf Titans"));
        assert!(html.contains("23.00"));
        assert!(html.contains("/entries/"), "resolved rows link the lineups");
        assert!(
            html.contains("ph-fill ph-trophy"),
            "winner gets a trophy: {html}"
        );
        assert_eq!(
            html.matches("ph-fill ph-trophy").count(),
            1,
            "only the winner gets a trophy"
        );
    }

    #[test]
    fn pre_results_rows_hide_points() {
        let html = page(vec![row("Turf Titans", None, None, false)], vec![])
            .render()
            .expect("render");
        assert!(html.contains("Turf Titans"));
        assert!(!html.contains("ph-fill ph-trophy"));
        assert!(!html.contains("0.00"), "no zero-filler points");
    }

    #[test]
    fn renders_no_entries_line() {
        let html = page(vec![], vec![]).render().expect("render");
        assert!(html.contains("No entries."));
    }

    #[test]
    fn upcoming_header_leads_with_lock_time_and_entry_count() {
        let html = ContestPage {
            upcoming: Some(upcoming()),
            ..page(
                vec![],
                vec![game("NE @ CIN", Some("Sun, Nov 23 · 1:00 PM"))],
            )
        }
        .render()
        .expect("render");
        assert!(
            html.contains("Kicks off Sun, Nov 23 · 1:00 PM ET"),
            "{html}"
        );
        assert!(html.contains("in 1 hour"), "{html}");
        assert!(html.contains("Entries · 0"), "{html}");
        assert!(
            html.contains("No entries yet. Be the first in."),
            "empty state invites, not shrugs: {html}"
        );
        assert!(
            !html.contains("No entries."),
            "no dead empty card pre-kickoff: {html}"
        );
    }

    #[test]
    fn upcoming_entry_count_tracks_rows() {
        let html = ContestPage {
            upcoming: Some(upcoming()),
            ..page(
                vec![row("Turf Titans", None, None, false)],
                vec![game("NE @ CIN", None)],
            )
        }
        .render()
        .expect("render");
        assert!(html.contains("Entries · 1"), "{html}");
        assert!(html.contains("Turf Titans"), "{html}");
        assert!(!html.contains("No entries yet."), "{html}");
        assert!(
            !html.contains("/entries/"),
            "no lineup peeking before kickoff: {html}"
        );
    }

    #[test]
    fn waiting_section_counts_the_teams_still_to_enter() {
        let html = ContestPage {
            upcoming: Some(upcoming()),
            waiting: vec![waiting("Mustangs", false), waiting("Turf Titans", false)],
            ..page(
                vec![row("Gridiron Giants", None, None, false)],
                vec![game("NE @ CIN", None)],
            )
        }
        .render()
        .expect("render");
        assert!(html.contains("Entries · 1"), "{html}");
        assert!(html.contains("Still to enter · 2"), "{html}");
        assert!(html.contains("Mustangs"), "{html}");
        assert!(html.contains("Turf Titans"), "{html}");
        assert!(
            !html.contains("of 22"),
            "one denominator per list, not a shared one: {html}"
        );
    }

    #[test]
    fn waiting_section_is_omitted_when_empty() {
        let html = ContestPage {
            upcoming: Some(upcoming()),
            ..page(vec![], vec![game("NE @ CIN", None)])
        }
        .render()
        .expect("render");
        assert!(!html.contains("Still to enter"), "{html}");
    }

    #[test]
    fn waiting_section_is_omitted_after_kickoff() {
        let html = ContestPage {
            waiting: vec![waiting("Mustangs", false)],
            ..page(vec![row("Turf Titans", Some(1), Some(23.0), true)], vec![])
        }
        .render()
        .expect("render");
        assert!(!html.contains("Still to enter"), "{html}");
        assert!(!html.contains("Mustangs"), "{html}");
    }

    #[test]
    fn the_viewers_own_waiting_row_links_to_the_lineup_editor() {
        let html = ContestPage {
            contest_id: 7,
            upcoming: Some(upcoming()),
            waiting: vec![waiting("Mustangs", true), waiting("Turf Titans", false)],
            ..page(vec![], vec![game("NE @ CIN", None)])
        }
        .render()
        .expect("render");
        assert!(html.contains(r#"href="/contests/7/draft-entry""#), "{html}");
        assert_eq!(
            html.matches("bg-primary/10").count(),
            1,
            "only the viewer's own waiting row is highlighted: {html}"
        );
    }

    #[test]
    fn games_section_lists_kickoffs_and_is_omitted_when_empty() {
        let with_games = page(
            vec![],
            vec![
                game("LA @ CAR", Some("Sat, Jan 10 · 4:30 PM")),
                game("SF @ PHI", None),
            ],
        )
        .render()
        .expect("render");
        assert!(with_games.contains("Games"));
        assert!(with_games.contains("LA @ CAR"));
        assert!(with_games.contains("Sat, Jan 10 · 4:30 PM"));

        let without = page(vec![], vec![]).render().expect("render");
        assert!(!without.contains("Games"), "section omitted: {without}");
    }

    #[test]
    fn renders_draft_action_enter_lineup() {
        let html = ContestPage {
            draft_action: Some(DraftAction {
                href: "/contests/7/draft-entry".to_string(),
                kind: LineupAction::Enter,
            }),
            upcoming: Some(upcoming()),
            ..page(
                vec![],
                vec![game("LA @ CAR", Some("Sat, Jan 10 · 4:30 PM"))],
            )
        }
        .render()
        .expect("render");
        assert!(html.contains(r#"href="/contests/7/draft-entry""#), "{html}");
        assert!(html.contains("Enter lineup"), "{html}");
    }

    #[test]
    fn renders_resume_label_when_a_draft_exists() {
        let html = ContestPage {
            draft_action: Some(DraftAction {
                href: "/contests/7/draft-entry".to_string(),
                kind: LineupAction::Resume,
            }),
            upcoming: Some(upcoming()),
            ..page(
                vec![],
                vec![game("LA @ CAR", Some("Sat, Jan 10 · 4:30 PM"))],
            )
        }
        .render()
        .expect("render");
        assert!(html.contains("Resume lineup"), "{html}");
    }

    #[test]
    fn upcoming_links_only_the_viewers_pinned_row() {
        let mine = EntryRow {
            id: EntryId(42),
            mine: true,
            ..row("Bootleg Bandits", None, None, false)
        };
        let html = ContestPage {
            upcoming: Some(upcoming()),
            ..page(
                vec![mine, row("Turf Titans", None, None, false)],
                vec![game("NE @ CIN", None)],
            )
        }
        .render()
        .expect("render");
        assert!(html.contains(r#"href="/entries/42""#), "{html}");
        assert_eq!(
            html.matches("/entries/").count(),
            1,
            "only the viewer's row links: {html}"
        );
        let mine_item = list_item_containing(&html, r#"href="/entries/42""#);
        let turf_item = list_item_containing(&html, "Turf Titans");
        assert!(mine_item.contains("bg-primary/10"), "own row highlighted");
        assert!(
            mine_item.contains("hover:bg-primary/20"),
            "own row strengthens on hover"
        );
        assert!(
            !turf_item.contains("bg-primary/10"),
            "other upcoming rows stay plain"
        );
        assert!(
            !turf_item.contains("hover:bg-primary/20"),
            "other upcoming rows have no hover"
        );
        assert_eq!(
            html.matches("bg-primary/10").count(),
            1,
            "only the owned row rests green: {html}"
        );
    }

    #[test]
    fn omits_draft_action_when_none() {
        let html = page(vec![], vec![]).render().expect("render");
        assert!(!html.contains("Enter lineup"), "{html}");
        assert!(!html.contains("Resume lineup"), "{html}");
    }

    #[test]
    fn an_entered_contest_shows_no_cta_and_no_entered_badge() {
        let html = page(vec![row("Sunday Funday", None, None, false)], vec![])
            .render()
            .expect("contest page renders");
        assert!(!html.contains("You're in"), "entered badge is gone: {html}");
        assert!(
            !html.contains("Enter lineup"),
            "no cta for unentered: {html}"
        );
        assert!(
            !html.contains("Resume lineup"),
            "no cta for unentered: {html}"
        );
    }
}
