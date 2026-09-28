use crate::chrome::{Chrome, Tab};
use crate::contests::service::{ContestState, SeasonSlates, contest_state};
use crate::contests::{Contest, ContestId, store as contests_store};
use crate::draft_entry::store as draft_entry_store;
use crate::entries::store as entries_store;
use crate::fantasy_teams::contest_entries::{SeasonView, season_view};
use crate::fantasy_teams::open_contests::{OpenAction, OpenContestRow, open_contests};
use crate::fantasy_teams::store as fantasy_teams;
use crate::fantasy_teams::{FantasyTeam, FantasyTeamId, MaybeTeam, OwnedTeam, TeamCreator, logo};
use crate::invites::store as invites;
use crate::leagues::CurrentLeague;
use crate::media::{self, Media};
use crate::scoring::service as scoring_service;
use crate::validation::{normalize_name, valid_name_chars};
use crate::web::{FormErrors, FormInput, Valid, redirect};
use crate::{AppError, AppState, form_view};
use askama::Template;
use axum::extract::multipart::MultipartError;
use axum::extract::{Multipart, Path, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use axum_htmx::HxRequest;
use garde::Validate;
use nfl_data::{Season, Week};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use tower_sessions::Session;

#[derive(Template, Default)]
#[template(path = "teams/new.html", blocks = ["form"])]
pub struct NewFantasyTeamTemplate {
    errors: FormErrors,
    name: String,
    owner_name: String,
}
form_view!(NewFantasyTeamTemplate);

#[derive(Deserialize, Validate)]
pub struct FantasyTeamInput {
    #[garde(length(min = 1, max = 60), custom(valid_name_chars))]
    name: String,
    #[garde(length(min = 1, max = 40), custom(valid_name_chars))]
    owner_name: String,
}

impl FormInput for FantasyTeamInput {
    type View = NewFantasyTeamTemplate;
    type Ctx = ();

    fn normalize(&mut self) {
        self.name = normalize_name(&self.name);
        self.owner_name = normalize_name(&self.owner_name);
    }

    fn to_view(&self, (): (), errors: FormErrors) -> Self::View {
        NewFantasyTeamTemplate {
            errors,
            name: self.name.clone(),
            owner_name: self.owner_name.clone(),
        }
    }
}

pub async fn new(_: TeamCreator) -> Result<impl IntoResponse, AppError> {
    Ok(Html(NewFantasyTeamTemplate::default().render()?).into_response())
}

pub async fn create(
    TeamCreator {
        user,
        league,
        invite_id,
    }: TeamCreator,
    HxRequest(is_htmx): HxRequest,
    session: Session,
    State(state): State<AppState>,
    Valid(form): Valid<FantasyTeamInput>,
) -> Result<impl IntoResponse, AppError> {
    let team = state
        .db
        .write_tx(async |conn| -> Result<FantasyTeam, AppError> {
            let team = fantasy_teams::create(
                &mut *conn,
                league.id,
                user.id,
                &form.name,
                &form.owner_name,
                user.is_admin, // site admin is always commish
            )
            .await?;
            if let Some(invite_id) = invite_id {
                invites::mark_accepted(&mut *conn, invite_id).await?;
            }
            Ok(team)
        })
        .await?;

    crate::leagues::clear_pending_creation(&session).await?;
    crate::leagues::set_active(&session, team.league_id).await?;

    Ok(redirect(is_htmx, "/"))
}

#[derive(Template)]
#[template(path = "teams/edit.html", blocks = ["form"])]
pub struct EditTeamTemplate {
    pub team_id: i64,
    errors: FormErrors,
    pub name: String,
    pub owner_name: String,
    pub chrome: Chrome,
}
form_view!(EditTeamTemplate);

/// The same fields as a new team, validated the same way, but rendered back
/// into the edit page for the route's team on error.
#[derive(Deserialize, Validate)]
#[serde(transparent)]
#[garde(transparent)]
pub struct EditTeamInput(#[garde(dive)] FantasyTeamInput);

impl FormInput for EditTeamInput {
    type View = EditTeamTemplate;
    type Ctx = OwnedTeam;

    fn normalize(&mut self) {
        self.0.normalize();
    }

    fn to_view(&self, OwnedTeam(team): OwnedTeam, errors: FormErrors) -> Self::View {
        EditTeamTemplate::new(
            team.id,
            self.0.name.clone(),
            self.0.owner_name.clone(),
            errors,
        )
    }
}

impl EditTeamTemplate {
    fn new(team_id: i64, name: String, owner_name: String, errors: FormErrors) -> Self {
        Self {
            team_id,
            errors,
            name,
            owner_name,
            chrome: team_chrome("Edit Team", team_id),
        }
    }
}

#[derive(Template)]
#[template(path = "teams/logo.html")]
pub struct TeamLogoTemplate {
    pub team_id: i64,
    pub name: String,
    pub logo: Option<Media>,
    pub chrome: Chrome,
}

impl TeamLogoTemplate {
    fn monogram(&self) -> String {
        super::monogram(&self.name)
    }
}

fn team_chrome(title: &str, team_id: i64) -> Chrome {
    Chrome::focused(title, format!("/teams/{team_id}"))
}

/// Multipart errors carry their own status (413 at the body limit, 5xx
/// mid-stream).
fn multipart_app_error(e: MultipartError) -> AppError {
    let status = e.status();
    if status == StatusCode::PAYLOAD_TOO_LARGE {
        AppError::PayloadTooLarge(e.to_string())
    } else if status.is_server_error() {
        AppError::Internal(e.to_string())
    } else {
        AppError::BadRequest(e.to_string())
    }
}

enum LogoSubmission {
    Upload(axum::body::Bytes),
    Remove,
}

impl LogoSubmission {
    async fn parse(mut mp: Multipart) -> Result<Self, AppError> {
        let mut logo = None;
        while let Some(field) = mp.next_field().await.map_err(multipart_app_error)? {
            match field.name() {
                Some("remove") => return Ok(Self::Remove),
                Some("logo") => logo = Some(field.bytes().await.map_err(multipart_app_error)?),
                _ => {}
            }
        }
        logo.map(Self::Upload)
            .ok_or_else(|| AppError::BadRequest("no logo file uploaded".into()))
    }
}

pub async fn edit_names(OwnedTeam(team): OwnedTeam) -> Result<Html<String>, AppError> {
    Ok(Html(
        EditTeamTemplate::new(team.id, team.name, team.owner_name, FormErrors::default())
            .render()?,
    ))
}

pub async fn update_names(
    OwnedTeam(team): OwnedTeam,
    HxRequest(is_htmx): HxRequest,
    State(state): State<AppState>,
    Valid(EditTeamInput(form)): Valid<EditTeamInput>,
) -> Result<Response, AppError> {
    state
        .db
        .write_tx(async |conn| {
            fantasy_teams::update_names(&mut *conn, team.id, &form.name, &form.owner_name).await
        })
        .await?;

    Ok(redirect(is_htmx, format!("/teams/{}", team.id)))
}

pub async fn logo_editor(
    OwnedTeam(team): OwnedTeam,
    State(state): State<AppState>,
) -> Result<Html<String>, AppError> {
    let logo = match team.logo_media_id {
        Some(id) => media::store::get(state.db.reader(), id).await?,
        None => None,
    };
    Ok(Html(
        TeamLogoTemplate {
            team_id: team.id,
            name: team.name,
            logo,
            chrome: team_chrome("Team Logo", team.id),
        }
        .render()?,
    ))
}

pub async fn update_logo(
    OwnedTeam(team): OwnedTeam,
    HxRequest(is_htmx): HxRequest,
    State(state): State<AppState>,
    mp: Multipart,
) -> Result<Response, AppError> {
    match LogoSubmission::parse(mp).await? {
        LogoSubmission::Upload(bytes) => {
            logo::upload_logo(&state.db, &state.config.media_dir, &team, &bytes).await?;
        }
        LogoSubmission::Remove => {
            logo::remove_logo(&state.db, &state.config.media_dir, &team).await?;
        }
    }
    Ok(redirect(is_htmx, format!("/teams/{}", team.id)))
}

#[derive(Template)]
#[template(path = "teams/show.html")]
pub struct TeamDetailTemplate {
    pub name: String,
    pub owner_name: String,
    pub logo: Option<String>,
    /// `Some(id)` when the viewer owns this team; the navbar links to
    /// `/teams/{id}/edit`, and the avatar links to `/teams/{id}/logo`.
    pub edit_team_id: Option<i64>,
    pub season: SeasonView,
    /// The owner's open contests — always empty for other viewers.
    pub open: Vec<OpenContestRow>,
    pub chrome: Chrome,
}

impl TeamDetailTemplate {
    fn monogram(&self) -> String {
        super::monogram(&self.name)
    }
}

/// Any league member's view of one fantasy team's season, best score first.
pub async fn show(
    State(state): State<AppState>,
    CurrentLeague(league): CurrentLeague,
    MaybeTeam(viewer_team): MaybeTeam,
    Path(team_id): Path<i64>,
) -> Result<Html<String>, AppError> {
    let team =
        entries_store::for_fantasy_team(state.db.reader(), league.id, FantasyTeamId(team_id))
            .await?
            .ok_or(AppError::NotFound)?;
    let edit_team_id = viewer_team.map(|t| t.id).filter(|id| *id == team_id);
    let now = state.now_eastern();

    // Finished contests only, and narrowed to this team's, so week_stats
    // loads just the weeks this page scores. A live contest is deliberately
    // absent: the season list's empty state promises "No scored contests
    // yet" and its total is a best-10 of final scores, so a partial score
    // never belonged there — the open-contests section above owns the live
    // row instead. The standings page draws its own scores from the same
    // helper.
    let season = Season(state.config.season);
    let slates = SeasonSlates::load(state.db.reader(), &state.nfl, season).await?;
    let contests = contests_store::all(state.db.reader()).await?;
    let contest_week: HashMap<ContestId, Week> = slates
        .finished_contest_weeks(now)
        .into_iter()
        .filter(|(contest, _)| team.entries.iter().any(|e| e.contest == *contest))
        .collect();
    let stats =
        scoring_service::week_stats(&state.nfl, season, contest_week.values().copied()).await?;
    let scores = scoring_service::score_entries(&team.entries, &contest_week, &stats);

    let names: HashMap<ContestId, &str> =
        contests.iter().map(|c| (c.id, c.name.as_str())).collect();
    let scored = scores
        .into_iter()
        .map(|(id, points)| {
            let name = names
                .get(&id)
                .expect("entries.contest_id FK: contest exists")
                .to_string();
            (Contest { id, name }, points)
        })
        .collect();

    // The open-contests section is the owner's: it links into a pre-lock
    // lineup and the draft editor, so other viewers get none of it.
    let open = match edit_team_id {
        Some(team_id) => {
            let slate_contests = slates.slate_contests(&contests);
            let entry_ids =
                entries_store::entry_ids_for_team(state.db.reader(), FantasyTeamId(team_id))
                    .await?;
            let mut resumable = HashSet::new();
            for c in &slate_contests {
                let enterable = c.first.is_some()
                    && !entry_ids.contains_key(&c.id)
                    && contest_state(c.first, c.last, now) == ContestState::Upcoming;
                if enterable
                    && draft_entry_store::by_keys(state.db.reader(), c.id, FantasyTeamId(team_id))
                        .await?
                        .is_some_and(|d| d.has_picks())
                {
                    resumable.insert(c.id);
                }
            }
            open_contests(&slate_contests, &entry_ids, &resumable, now)
        }
        None => Vec::new(),
    };

    Ok(Html(
        TeamDetailTemplate {
            chrome: Chrome::tabbed(
                league.name.clone(),
                if edit_team_id.is_some() {
                    Tab::MyTeam
                } else {
                    Tab::Standings
                },
            ),
            name: team.name,
            owner_name: team.owner_name,
            logo: team.logo.map(|m| m.url()),
            edit_team_id,
            season: season_view(scored),
            open,
        }
        .render()?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contests::ContestId;
    use crate::fantasy_teams::contest_entries::ContestRow;

    fn contest_row(name: &str, points: f64, counts: bool, score_to_beat: bool) -> ContestRow {
        ContestRow {
            contest: ContestId(4),
            name: name.to_string(),
            points,
            counts,
            score_to_beat,
        }
    }

    #[test]
    fn renders_header_rows_and_score_to_beat() {
        let html = TeamDetailTemplate {
            chrome: Chrome::tabbed("Gridiron Giants", Tab::Standings),
            name: "Gridiron Giants".to_string(),
            owner_name: "Mike".to_string(),
            logo: None,
            edit_team_id: None,
            season: SeasonView {
                rows: vec![
                    contest_row("Week 2", 23.0, true, false),
                    contest_row("Week 1", 18.0, true, true),
                    contest_row("Week 3", 5.0, false, false),
                ],
                total: Some(41.0),
            },
            open: Vec::new(),
        }
        .render()
        .expect("render");
        assert!(html.contains("Gridiron Giants"));
        assert!(html.contains("Mike"));
        assert!(html.contains("41.00"), "season total: {html}");
        assert!(html.contains("season total"), "label under 11 rows: {html}");
        assert!(html.contains("23.00"));
        assert!(html.contains("Score to beat"), "tenth-row label: {html}");
        assert!(html.contains("Does not count"), "muted tail label: {html}");
        assert!(html.contains("GG"), "monogram fallback: {html}");
        assert!(
            !html.contains(r#"href="/teams/"#),
            "non-owner team edit action: {html}"
        );
    }

    #[test]
    fn contest_rows_link_to_the_contest_details_page() {
        let html = TeamDetailTemplate {
            chrome: Chrome::tabbed("Gridiron Giants", Tab::Standings),
            name: "Gridiron Giants".to_string(),
            owner_name: "Mike".to_string(),
            logo: None,
            edit_team_id: None,
            season: SeasonView {
                rows: vec![contest_row("Week 2", 23.0, true, false)],
                total: Some(23.0),
            },
            open: Vec::new(),
        }
        .render()
        .expect("render");
        assert!(
            html.contains(r#"href="/contests/4""#),
            "contest row links to the contest details page: {html}"
        );
    }

    #[test]
    fn header_label_flips_to_best_ten_when_truncation_bites() {
        let rows: Vec<ContestRow> = (1..=11)
            .map(|i| contest_row(&format!("Week {i}"), i as f64, i <= 10, i == 10))
            .collect();
        let html = TeamDetailTemplate {
            chrome: Chrome::tabbed("Gridiron Giants", Tab::Standings),
            name: "Gridiron Giants".to_string(),
            owner_name: "Mike".to_string(),
            logo: None,
            edit_team_id: None,
            season: SeasonView {
                rows,
                total: Some(55.0),
            },
            open: Vec::new(),
        }
        .render()
        .expect("render");
        assert!(html.contains("best-10 total"), "{html}");
        assert!(!html.contains("season total"), "{html}");
    }

    #[test]
    fn renders_logo_image_instead_of_monogram_when_present() {
        let html = TeamDetailTemplate {
            chrome: Chrome::tabbed("Gridiron Giants", Tab::Standings),
            name: "Gridiron Giants".to_string(),
            owner_name: "Mike".to_string(),
            logo: Some("/media/7.png?v=1".to_string()),
            edit_team_id: None,
            season: SeasonView {
                rows: vec![],
                total: None,
            },
            open: Vec::new(),
        }
        .render()
        .expect("render");
        assert!(
            html.contains(r#"src="/media/7.png?v=1""#),
            "logo img: {html}"
        );
        assert!(!html.contains("GG"), "no monogram fallback: {html}");
    }

    #[test]
    fn renders_empty_state_with_blank_total() {
        let html = TeamDetailTemplate {
            chrome: Chrome::tabbed("Bench Warmers", Tab::Standings),
            name: "Bench Warmers".to_string(),
            owner_name: "Lee".to_string(),
            logo: None,
            edit_team_id: None,
            season: SeasonView {
                rows: vec![],
                total: None,
            },
            open: Vec::new(),
        }
        .render()
        .expect("render");
        assert!(html.contains("No scored contests yet."), "{html}");
        assert!(!html.contains("season total"), "no total block: {html}");
    }

    #[test]
    fn form_accepts_valid() {
        let form = FantasyTeamInput {
            name: "4th & Long".to_string(),
            owner_name: "José".to_string(),
        };
        assert!(form.validate().is_ok());
    }

    #[test]
    fn page_and_fragment_render() {
        NewFantasyTeamTemplate::default()
            .render()
            .expect("new page");
        NewFantasyTeamTemplate {
            errors: FormErrors::default(),
            name: "Gridiron Giants".to_string(),
            owner_name: "Mike".to_string(),
        }
        .as_form()
        .render()
        .expect("new fragment");
        let edit = EditTeamTemplate::new(
            7,
            "Gridiron Giants".to_string(),
            "Mike".to_string(),
            FormErrors::default(),
        );
        edit.render().expect("edit page");
        edit.as_form().render().expect("edit fragment");
    }

    #[test]
    fn team_logo_page_renders_with_and_without_logo() {
        let with = TeamLogoTemplate {
            team_id: 7,
            name: "Champs".to_string(),
            logo: Some(Media::fixture(
                "7.png",
                time::macros::datetime!(2026-08-09 12:00:00),
            )),
            chrome: Chrome::focused("Team Logo", "/teams/7"),
        }
        .render()
        .expect("with logo");
        assert!(with.contains("/media/7.png?v="), "logo img: {with}");
        assert!(
            with.contains(r#"name="remove" formnovalidate"#),
            "remove button skips the required file input: {with}"
        );

        let without = TeamLogoTemplate {
            team_id: 7,
            name: "Champs".to_string(),
            logo: None,
            chrome: Chrome::focused("Team Logo", "/teams/7"),
        }
        .render()
        .expect("no logo");
        assert!(
            without.contains(r#"action="/teams/7/logo""#),
            "upload form: {without}"
        );
        assert!(
            without.contains(r#"id="team-logo""#),
            "labeled file input: {without}"
        );
        assert!(
            without.contains(
                r#"<button type="submit" class="btn btn-primary btn-sm">Upload logo</button>"#
            ),
            "upload button keeps the required file check: {without}"
        );
        assert!(
            !without.contains(r#"name="remove""#),
            "no remove control without a logo: {without}"
        );
    }

    #[test]
    fn form_rejects_emoji_in_either_field() {
        let bad_team = FantasyTeamInput {
            name: "🏈Team".to_string(),
            owner_name: "Mike".to_string(),
        };
        assert!(bad_team.validate().is_err());
        let bad_owner = FantasyTeamInput {
            name: "Champs".to_string(),
            owner_name: "★Mike".to_string(),
        };
        assert!(bad_owner.validate().is_err());
    }
}
