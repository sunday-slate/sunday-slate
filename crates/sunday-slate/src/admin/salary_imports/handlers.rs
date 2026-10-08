use askama::Template;
use axum::Router;
use axum::extract::{Multipart, Path, State};
use axum::response::{Html, IntoResponse, Redirect};
use axum::routing::{get, post};
use axum_messages::Messages;
use serde::Deserialize;

use crate::admin::salary_imports::candidates::{
    ResultsFragment, candidate, is_skill_position, team_default_candidates,
};
use crate::admin::salary_imports::model::RowGroup;
use crate::admin::salary_imports::store as import_store;
use crate::admin::salary_imports::view::{
    GroupQuery, IndexTemplate, ReviewTemplate, build_triage, import_chrome, imports_chrome,
    review_group,
};
use crate::admin::salary_imports::{self as import, PlanError};
use crate::auth::AdminUser;
use crate::{AppError, AppState};

/// Routes for the admin salary-import UI. Mounted under the admin router, which
/// applies the `AdminUser` gate for the whole subtree.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(index).post(upload))
        .route("/{id}", get(review))
        .route("/{id}/resolved", get(review_resolved))
        .route("/{id}/skipped", get(review_skipped))
        .route("/{id}/triage", get(triage))
        .route("/{id}/rows/{row_id}", post(resolve))
        .route("/{id}/rows/{row_id}/undo", post(undo))
        .route("/{id}/rows/{row_id}/players", get(row_players))
        .route("/{id}/commit", post(commit))
        .route("/{id}/discard", post(discard))
}

pub async fn index(State(state): State<AppState>) -> Result<impl IntoResponse, AppError> {
    let imports = import_store::list_recent(state.db.reader(), 25).await?;
    Ok(Html(IndexTemplate::new(imports).render()?))
}

struct UploadedCsv {
    bytes: Vec<u8>,
    filename: Option<String>,
}

/// Read the first `csv` file part, keeping its client-provided filename as metadata.
async fn read_csv_part(mut mp: Multipart) -> Result<UploadedCsv, AppError> {
    while let Some(field) = mp
        .next_field()
        .await
        .map_err(|e| AppError::BadRequest(e.to_string()))?
    {
        if field.name() == Some("csv") {
            let filename = field.file_name().map(str::to_owned);
            let bytes = field
                .bytes()
                .await
                .map_err(|e| AppError::BadRequest(e.to_string()))?
                .to_vec();
            return Ok(UploadedCsv { bytes, filename });
        }
    }
    Err(AppError::BadRequest("no csv file uploaded".into()))
}

pub async fn upload(
    AdminUser(user): AdminUser,
    State(state): State<AppState>,
    mp: Multipart,
) -> Result<axum::response::Response, AppError> {
    let upload = read_csv_part(mp).await?;
    let season = state.config.season;

    let plan = match import::plan_upload(
        &state.db,
        &state.nfl,
        &upload.bytes,
        upload.filename.as_deref(),
        season,
    )
    .await
    {
        Ok(p) => p,
        Err(e) => {
            let msg = match e {
                PlanError::Empty => "That CSV had no salary rows.".to_string(),
                PlanError::UnresolvableSlate => {
                    "Couldn't match these games to a single NFL week.".to_string()
                }
                PlanError::Csv(m) => format!("Couldn't read that CSV: {m}"),
                PlanError::Data(m) => format!("Couldn't load NFL data: {m}"),
                PlanError::Upload(m) => return Err(AppError::Internal(m)),
                PlanError::Input(m) => return Err(AppError::Internal(m)),
            };
            let imports = import_store::list_recent(state.db.reader(), 25).await?;
            let view = IndexTemplate {
                imports,
                error: Some(msg),
                offending: vec![],
                chrome: imports_chrome(),
            };
            return Ok(Html(view.render()?).into_response());
        }
    };

    for diagnostic in &plan.diagnostics {
        tracing::warn!(
            target: "sunday_slate::salary_imports",
            row_number = diagnostic.row_number,
            field = %diagnostic.field,
            message = %diagnostic.message,
            "FanDuel salary row diagnostic"
        );
    }

    if !plan.offending.is_empty() {
        let imports = import_store::list_recent(state.db.reader(), 25).await?;
        let view = IndexTemplate {
            imports,
            error: None,
            offending: plan.offending,
            chrome: imports_chrome(),
        };
        return Ok(Html(view.render()?).into_response());
    }

    let id = import_store::stage(&state.db, &plan, Some(user.id)).await?;
    Ok(Redirect::to(&format!("/admin/salary-imports/{id}")).into_response())
}

pub async fn commit(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    messages: Messages,
) -> Result<impl IntoResponse, AppError> {
    let report = import::commit_staged(&state.db, id)
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;
    messages.success(format!("Imported {} salaries.", report.salaries));
    Ok(Redirect::to("/admin/salary-imports").into_response())
}

pub async fn discard(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    messages: Messages,
) -> Result<impl IntoResponse, AppError> {
    state
        .db
        .write_tx::<_, (), sqlx::Error>(async |conn| import_store::delete(&mut *conn, id).await)
        .await?;
    messages.success("Import discarded.");
    Ok(Redirect::to("/admin/salary-imports").into_response())
}

pub async fn review(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<impl IntoResponse, AppError> {
    review_group(state, id, RowGroup::Unmatched).await
}

pub async fn review_resolved(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<impl IntoResponse, AppError> {
    review_group(state, id, RowGroup::Resolved).await
}

pub async fn review_skipped(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<impl IntoResponse, AppError> {
    review_group(state, id, RowGroup::Skipped).await
}

pub async fn triage(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    axum::extract::Query(query): axum::extract::Query<GroupQuery>,
) -> Result<impl IntoResponse, AppError> {
    match build_triage(&state, id, query.group()).await? {
        Some(triage) => Ok(Html(
            ReviewTemplate {
                triage,
                chrome: import_chrome(id),
            }
            .as_triage()
            .render()?,
        )
        .into_response()),
        None => Err(AppError::NotFound),
    }
}

#[derive(Deserialize)]
pub struct ResolveForm {
    #[serde(default)]
    gsis_player_id: String,
    #[serde(default)]
    skip: Option<String>,
}

/// A decision moves the row out of whatever group the admin is looking at, so
/// the response is that whole group re-rendered — counts included — rather than
/// the row, which no longer belongs on screen.
pub async fn resolve(
    State(state): State<AppState>,
    Path((import_id, row_id)): Path<(i64, i64)>,
    axum::extract::Query(query): axum::extract::Query<GroupQuery>,
    axum::Form(form): axum::Form<ResolveForm>,
) -> Result<impl IntoResponse, AppError> {
    let pick = if form.skip.is_some() || form.gsis_player_id.is_empty() {
        None
    } else {
        Some(form.gsis_player_id)
    };
    state
        .db
        .write_tx::<_, (), sqlx::Error>(async |conn| {
            import_store::resolve_row(&mut *conn, import_id, row_id, pick.as_deref()).await
        })
        .await?;

    let Some(triage) = build_triage(&state, import_id, query.group()).await? else {
        return Err(AppError::NotFound);
    };
    let chrome = import_chrome(import_id);
    Ok(Html(ReviewTemplate { triage, chrome }.as_triage().render()?).into_response())
}

/// Return a decided row to the group that needs work. Scoped to decided rows by
/// [`import_store::unresolve_row`], so an auto-matched row cannot be undone.
pub async fn undo(
    State(state): State<AppState>,
    Path((import_id, row_id)): Path<(i64, i64)>,
    axum::extract::Query(query): axum::extract::Query<GroupQuery>,
) -> Result<impl IntoResponse, AppError> {
    state
        .db
        .write_tx::<_, (), sqlx::Error>(async |conn| {
            import_store::unresolve_row(&mut *conn, import_id, row_id).await
        })
        .await?;
    let Some(triage) = build_triage(&state, import_id, query.group()).await? else {
        return Err(AppError::NotFound);
    };
    let chrome = import_chrome(import_id);
    Ok(Html(ReviewTemplate { triage, chrome }.as_triage().render()?).into_response())
}

#[derive(Deserialize)]
pub struct PlayerQuery {
    #[serde(default)]
    q: String,
    /// `all` widens past the row's week; anything else keeps the week scoping.
    #[serde(default)]
    scope: Option<String>,
}

/// Candidate list for one row, re-rendered on every keystroke.
///
/// Typing filters the week roster the row already shows rather than replacing it
/// with a global result set: nearly every unmatched row is a player who was on
/// that team that week, and a silent swap left the admin unable to tell which
/// list he was reading. Reaching another team is the explicit `scope=all` widen,
/// which the fragment offers as a button.
///
/// The two floors differ on purpose. A roster is ~30 names, so one character is
/// a useful filter; the `players` table is ~1800, so a one-character global
/// search returns nearly all of it and is not worth running. Either way an empty
/// query returns the row's full week roster rather than an empty list — the
/// response replaces whatever the admin was looking at, so an empty body
/// silently destroys the candidates the row was rendered with.
pub async fn row_players(
    State(state): State<AppState>,
    Path((import_id, row_id)): Path<(i64, i64)>,
    axum::extract::Query(query): axum::extract::Query<PlayerQuery>,
) -> Result<impl IntoResponse, AppError> {
    let staged = import_store::rows_for(state.db.reader(), import_id).await?;
    let Some(row) = staged.iter().find(|r| r.id == row_id) else {
        return Err(AppError::NotFound);
    };

    let needle = query.q.trim().to_lowercase();
    let widened = query.scope.as_deref() == Some("all");
    // Under the global floor nothing is searched at all, which the fragment
    // reports as such. Rendering it as an empty result told an admin who had
    // just widened from an empty box that his player did not exist.
    let too_short = widened && needle.len() < 2;

    let (candidates, scope_label) = if widened {
        let hits = if too_short {
            vec![]
        } else {
            let all = state
                .nfl
                .players()
                .await
                .map_err(|e| AppError::Internal(e.to_string()))?;
            all.iter()
                .filter(|p| p.full_name.to_lowercase().contains(&needle))
                .filter(|p| is_skill_position(p.position.as_deref()))
                .map(candidate)
                .collect()
        };
        (hits, "across all teams and weeks".to_string())
    } else {
        let (base, source) = team_default_candidates(&state, import_id, row).await?;
        let label = source.label(&row.fd_team);
        let hits = if needle.is_empty() {
            base
        } else {
            base.into_iter()
                .filter(|c| c.label.to_lowercase().contains(&needle))
                .collect()
        };
        (hits, label)
    };

    Ok(Html(
        ResultsFragment::new(
            import_id,
            row_id,
            candidates,
            scope_label,
            widened,
            too_short,
        )
        .render()?,
    )
    .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_renders() {
        IndexTemplate::new(vec![]).render().expect("index");
    }

    use crate::tests::TestApp;
    use crate::tests::factories::{self, UserOptions};
    use http::StatusCode;

    #[tokio::test]
    async fn non_admin_cannot_open_salaries() {
        let app = TestApp::new().await;
        let user = factories::user(
            &app.pool,
            UserOptions {
                is_admin: false,
                ..Default::default()
            },
        )
        .await;
        app.login_as(&user.user).await;
        app.get("/admin/salary-imports")
            .await
            .assert_status(StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn admin_sees_salaries_index() {
        let app = TestApp::new().await;
        let admin = factories::user(
            &app.pool,
            UserOptions {
                is_admin: true,
                ..Default::default()
            },
        )
        .await;
        app.login_as(&admin.user).await;
        app.get("/admin/salary-imports").await.assert_status_ok();
    }

    /// `/admin/` is exempt from the bootstrap onboarding redirect, but must stay
    /// behind `login_required`: an anonymous request redirects to /login, never
    /// reaching the handler.
    #[tokio::test]
    async fn unauthenticated_is_redirected_to_login() {
        let app = TestApp::new().await;
        let resp = app.get("/admin/salary-imports").await;
        assert!(
            resp.status_code().is_redirection(),
            "expected a redirect, got {}",
            resp.status_code()
        );
        assert!(
            resp.header("location")
                .to_str()
                .unwrap()
                .starts_with("/login"),
            "anonymous /admin/salary-imports must redirect to /login"
        );
    }

    use crate::admin::salary_imports::candidates::Candidate;
    use crate::admin::salary_imports::model::RowState;
    use crate::admin::salary_imports::store as import_store;
    use crate::admin::salary_imports::view::{GroupTab, ReviewRow, ReviewTemplate, TriageView};
    use axum_test::multipart::{MultipartForm, Part};
    use nfl_data::{
        Game, Player, Season, SeasonType, TeamAbbr as NflTeamAbbr, Week, WeeklyRosterEntry,
    };

    fn game(id: &str, away: &str, home: &str) -> Game {
        Game {
            gsis_game_id: id.into(),
            season: Season(2025),
            week: Week(1),
            season_type: SeasonType::Reg,
            kickoff: None,
            home_team: NflTeamAbbr(home.into()),
            away_team: NflTeamAbbr(away.into()),
            home_score: None,
            away_score: None,
        }
    }
    fn player(gsis: &str, name: &str, team: &str) -> Player {
        player_at(gsis, name, "WR", team)
    }
    fn player_at(gsis: &str, name: &str, pos: &str, team: &str) -> Player {
        player_named(gsis, name, "", pos, team)
    }
    fn player_named(gsis: &str, name: &str, last: &str, pos: &str, team: &str) -> Player {
        Player {
            gsis_id: gsis.into(),
            espn_id: None,
            full_name: name.into(),
            first_name: None,
            last_name: Some(last.into()).filter(|l: &String| !l.is_empty()),
            position: Some(pos.into()),
            latest_team: Some(NflTeamAbbr(team.into())),
            headshot_url: None,
        }
    }
    const HDR: &str = "\"Id\",\"Position\",\"First Name\",\"Nickname\",\"Last Name\",\"FPPG\",\"Played\",\"Salary\",\"Game\",\"Team\",\"Opponent\",\"Injury Indicator\",\"Injury Details\"";

    #[tokio::test]
    async fn upload_stages_and_redirects_to_review() {
        let app = TestApp::new().await;
        app.login_admin().await;
        app.nfl
            .seed_for_test(
                &[player("00-1", "Josh Allen", "BUF")],
                &[game("2025_01_BUF_NYJ", "BUF", "NYJ")],
            )
            .await
            .unwrap();

        let csv = format!(
            "{HDR}\n\"1-100\",\"QB\",\"Josh\",\"\",\"Allen\",\"0\",\"0\",\"8000\",\"BUF@NYJ\",\"BUF\",\"NYJ\",\"\",\"\"\n\"1-200\",\"WR\",\"Nobody\",\"\",\"Here\",\"0\",\"0\",\"5000\",\"BUF@NYJ\",\"BUF\",\"NYJ\",\"\",\"\"\n"
        );
        let form = MultipartForm::new().add_part(
            "csv",
            Part::bytes(csv.into_bytes())
                .file_name("s.csv")
                .mime_type("text/csv"),
        );

        let resp = app
            .server
            .post("/admin/salary-imports")
            .multipart(form)
            .await;
        resp.assert_status(StatusCode::SEE_OTHER);
        let loc = resp.header("location").to_str().unwrap().to_string();
        assert!(loc.starts_with("/admin/salary-imports/"), "loc = {loc}");

        // One batch, two staged rows, one unmatched.
        let imports = import_store::list_recent(&app.pool, 10).await.unwrap();
        assert_eq!(imports.len(), 1);
        assert_eq!(
            import_store::unmatched_count(&app.pool, imports[0].id)
                .await
                .unwrap(),
            1
        );
    }

    #[tokio::test]
    async fn upload_with_offending_rows_shows_them_and_stages_nothing() {
        let app = TestApp::new().await;
        app.login_admin().await;
        app.nfl
            .seed_for_test(&[], &[game("2025_01_BUF_NYJ", "BUF", "NYJ")])
            .await
            .unwrap();

        let csv = format!(
            "{HDR}\n\"1-400\",\"K\",\"Bad\",\"\",\"Pos\",\"0\",\"0\",\"5000\",\"BUF@NYJ\",\"BUF\",\"NYJ\",\"\",\"\"\n"
        );
        let form = MultipartForm::new().add_part(
            "csv",
            Part::bytes(csv.into_bytes())
                .file_name("s.csv")
                .mime_type("text/csv"),
        );

        let resp = app
            .server
            .post("/admin/salary-imports")
            .multipart(form)
            .await;
        resp.assert_status_ok(); // re-renders the upload page, no redirect
        assert!(resp.text().contains("couldn't be processed"));
        assert!(
            import_store::list_recent(&app.pool, 10)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn review_lists_unmatched_with_candidates() {
        let app = TestApp::new().await;
        app.login_admin().await;
        app.nfl
            .seed_for_test(
                &[
                    player("00-1", "Josh Allen", "BUF"),
                    player("00-2", "Some Buffalo Guy", "BUF"),
                ],
                &[game("2025_01_BUF_NYJ", "BUF", "NYJ")],
            )
            .await
            .unwrap();
        let csv = format!(
            "{HDR}\n\"1-200\",\"WR\",\"Nobody\",\"\",\"Here\",\"0\",\"0\",\"5000\",\"BUF@NYJ\",\"BUF\",\"NYJ\",\"\",\"\"\n"
        );
        let form = MultipartForm::new().add_part(
            "csv",
            Part::bytes(csv.into_bytes())
                .file_name("s.csv")
                .mime_type("text/csv"),
        );
        let loc = app
            .server
            .post("/admin/salary-imports")
            .multipart(form)
            .await
            .header("location")
            .to_str()
            .unwrap()
            .to_string();

        let resp = app.get(&loc).await;
        resp.assert_status_ok();
        let body = resp.text();
        // The page opens on the group that needs work, named by its tab.
        assert!(body.contains("Needs a decision"));

        // Candidates are no longer pre-rendered — they load when the row's
        // search input is focused, via the /players endpoint.
        let import_id: i64 = loc.rsplit('/').next().unwrap().parse().unwrap();
        let row_id = import_store::rows_for(&app.pool, import_id).await.unwrap()[0].id;
        let players = app
            .get(&format!(
                "/admin/salary-imports/{import_id}/rows/{row_id}/players"
            ))
            .await;
        players.assert_status_ok();
        assert!(players.text().contains("Some Buffalo Guy (")); // candidate rendered
    }

    /// The 80 pickers were 415 KB — 57% of the page — for rows the admin may never
    /// touch. The row now ships an empty container that fills on focus.
    #[tokio::test]
    async fn unmatched_rows_ship_no_candidates_until_asked() {
        let app = TestApp::new().await;
        app.login_admin().await;
        app.nfl
            .seed_for_test(&[], &[game("2025_01_BUF_NYJ", "BUF", "NYJ")])
            .await
            .unwrap();
        app.nfl
            .seed_weekly_roster_for_test(&[weekly("00-1", "Roster Guy", "WR", "BUF", 1)])
            .await
            .unwrap();
        let (import_id, row_id) = stage_one_unmatched(&app).await;

        let body = app
            .get(&format!("/admin/salary-imports/{import_id}"))
            .await
            .text();
        assert!(
            !body.contains("Roster Guy"),
            "no candidates in the initial page, body:\n{body}"
        );
        assert!(
            body.contains(&format!("id=\"row-{row_id}-results\"")),
            "but the container is there for htmx to fill"
        );
        assert!(
            body.contains("focus once"),
            "and the input loads it on focus"
        );

        // Focusing issues exactly this request.
        let loaded = app
            .get(&format!(
                "/admin/salary-imports/{import_id}/rows/{row_id}/players"
            ))
            .await;
        loaded.assert_status_ok();
        assert!(
            loaded.text().contains("Roster Guy"),
            "which returns the week roster"
        );
    }

    #[tokio::test]
    async fn resolve_row_marks_resolved_and_returns_fragment() {
        let app = TestApp::new().await;
        app.login_admin().await;
        app.nfl
            .seed_for_test(
                &[player("00-1", "Real Player", "BUF")],
                &[game("2025_01_BUF_NYJ", "BUF", "NYJ")],
            )
            .await
            .unwrap();
        let csv = format!(
            "{HDR}\n\"1-200\",\"WR\",\"Nobody\",\"\",\"Here\",\"0\",\"0\",\"5000\",\"BUF@NYJ\",\"BUF\",\"NYJ\",\"\",\"\"\n"
        );
        let form = MultipartForm::new().add_part(
            "csv",
            Part::bytes(csv.into_bytes())
                .file_name("s.csv")
                .mime_type("text/csv"),
        );
        let loc = app
            .server
            .post("/admin/salary-imports")
            .multipart(form)
            .await
            .header("location")
            .to_str()
            .unwrap()
            .to_string();
        let import_id = loc.rsplit('/').next().unwrap().parse::<i64>().unwrap();
        let row_id = import_store::rows_for(&app.pool, import_id).await.unwrap()[0].id;

        let resp = app
            .post_htmx(
                &format!("/admin/salary-imports/{import_id}/rows/{row_id}"),
                "gsis_player_id=00-1",
            )
            .await;
        resp.assert_status_ok();
        // No `group` param, so the response is the unmatched group — which the
        // row has just left.
        let body = resp.text();
        assert!(
            body.contains("id=\"triage\""),
            "response is the triage fragment, body was:\n{body}"
        );
        assert!(
            body.contains("Every row has a decision."),
            "row left the unmatched group, body was:\n{body}"
        );
        assert_eq!(
            import_store::unmatched_count(&app.pool, import_id)
                .await
                .unwrap(),
            0
        );
    }

    /// Upload the one-row "Nobody Here / BUF" CSV and return `(import_id, row_id)`
    /// for the single staged row, which is always Unmatched.
    async fn stage_one_unmatched(app: &TestApp) -> (i64, i64) {
        let csv = format!(
            "{HDR}\n\"1-200\",\"WR\",\"Nobody\",\"\",\"Here\",\"0\",\"0\",\"5000\",\"BUF@NYJ\",\"BUF\",\"NYJ\",\"\",\"\"\n"
        );
        let form = MultipartForm::new().add_part(
            "csv",
            Part::bytes(csv.into_bytes())
                .file_name("s.csv")
                .mime_type("text/csv"),
        );
        let loc = app
            .server
            .post("/admin/salary-imports")
            .multipart(form)
            .await
            .header("location")
            .to_str()
            .unwrap()
            .to_string();
        let import_id = loc.rsplit('/').next().unwrap().parse::<i64>().unwrap();
        let row_id = import_store::rows_for(&app.pool, import_id).await.unwrap()[0].id;
        (import_id, row_id)
    }

    fn weekly(gsis: &str, name: &str, pos: &str, team: &str, week: u8) -> WeeklyRosterEntry {
        weekly_named(gsis, name, "", pos, team, week)
    }
    fn weekly_named(
        gsis: &str,
        name: &str,
        last: &str,
        pos: &str,
        team: &str,
        week: u8,
    ) -> WeeklyRosterEntry {
        WeeklyRosterEntry {
            season: Season(2025),
            week: Week(week),
            team: NflTeamAbbr(team.into()),
            gsis_id: Some(gsis.into()),
            espn_id: None,
            full_name: name.into(),
            last_name: Some(last.into()).filter(|l: &String| !l.is_empty()),
            position: Some(pos.into()),
        }
    }

    /// The admin matches by eye, so the row has to say what FanDuel called the
    /// player, not just his team and price.
    #[tokio::test]
    async fn review_row_shows_the_fanduel_position() {
        let app = TestApp::new().await;
        app.login_admin().await;
        app.nfl
            .seed_for_test(&[], &[game("2025_01_BUF_NYJ", "BUF", "NYJ")])
            .await
            .unwrap();
        let (import_id, _) = stage_one_unmatched(&app).await;

        let resp = app.get(&format!("/admin/salary-imports/{import_id}")).await;
        resp.assert_status_ok();
        let body = resp.text();
        assert!(
            body.contains("BUF · WR · $5000"),
            "row header carries the FanDuel position, body was:\n{body}"
        );
    }

    /// The point of the weekly roster: candidates are the players on that team
    /// in that week. A player whose *latest* team is BUF but who was elsewhere
    /// in week 1 must not be offered.
    #[tokio::test]
    async fn candidates_come_from_that_weeks_roster() {
        let app = TestApp::new().await;
        app.login_admin().await;
        app.nfl
            .seed_for_test(
                &[
                    player("00-1", "Week One Bill", "BUF"),
                    player("00-2", "Signed Later", "BUF"),
                ],
                &[game("2025_01_BUF_NYJ", "BUF", "NYJ")],
            )
            .await
            .unwrap();
        app.nfl
            .seed_weekly_roster_for_test(&[
                weekly("00-1", "Week One Bill", "WR", "BUF", 1),
                // Same team, but not until week 5.
                weekly("00-2", "Signed Later", "WR", "BUF", 5),
            ])
            .await
            .unwrap();
        let (import_id, row_id) = stage_one_unmatched(&app).await;

        let resp = app
            .get(&format!(
                "/admin/salary-imports/{import_id}/rows/{row_id}/players"
            ))
            .await;
        resp.assert_status_ok();
        let body = resp.text();
        assert!(body.contains("Week One Bill"), "week 1 roster is offered");
        assert!(
            !body.contains("Signed Later"),
            "a player not on the week 1 roster must not be offered, body was:\n{body}"
        );
    }

    /// FanDuel salary rows are only ever skill players, so linemen and defenders
    /// are noise in a list the admin has to read.
    #[tokio::test]
    async fn defenders_and_linemen_are_not_offered() {
        let app = TestApp::new().await;
        app.login_admin().await;
        app.nfl
            .seed_for_test(&[], &[game("2025_01_BUF_NYJ", "BUF", "NYJ")])
            .await
            .unwrap();
        app.nfl
            .seed_weekly_roster_for_test(&[
                weekly("00-1", "Skill Guy", "WR", "BUF", 1),
                weekly("00-2", "Corner Guy", "DB", "BUF", 1),
                weekly("00-3", "Tackle Guy", "OL", "BUF", 1),
                weekly("00-4", "Rusher Guy", "DL", "BUF", 1),
                weekly("00-5", "Backer Guy", "LB", "BUF", 1),
                weekly("00-6", "Punter Guy", "P", "BUF", 1),
            ])
            .await
            .unwrap();
        let (import_id, row_id) = stage_one_unmatched(&app).await;

        let resp = app
            .get(&format!(
                "/admin/salary-imports/{import_id}/rows/{row_id}/players"
            ))
            .await;
        resp.assert_status_ok();
        let body = resp.text();
        assert!(body.contains("Skill Guy"), "skill players stay");
        for excluded in [
            "Corner Guy",
            "Tackle Guy",
            "Rusher Guy",
            "Backer Guy",
            "Punter Guy",
        ] {
            assert!(!body.contains(excluded), "{excluded} must be filtered out");
        }
    }

    /// The same filter applies to the widened search, which reads the
    /// finer-grained `players` position vocabulary (CB/OT/DE rather than DB/OL/DL).
    #[tokio::test]
    async fn search_also_filters_out_defenders_and_linemen() {
        let app = TestApp::new().await;
        app.login_admin().await;
        app.nfl
            .seed_for_test(
                &[
                    player_at("00-1", "Bobby Skill", "TE", "KC"),
                    player_at("00-2", "Bobby Corner", "CB", "KC"),
                    player_at("00-3", "Bobby Tackle", "OT", "KC"),
                    player_at("00-4", "Bobby Edge", "DE", "KC"),
                ],
                &[game("2025_01_BUF_NYJ", "BUF", "NYJ")],
            )
            .await
            .unwrap();
        let (import_id, row_id) = stage_one_unmatched(&app).await;

        let resp = app
            .get(&format!(
                "/admin/salary-imports/{import_id}/rows/{row_id}/players?q=Bobby&scope=all"
            ))
            .await;
        resp.assert_status_ok();
        let body = resp.text();
        assert!(body.contains("Bobby Skill"), "skill players are searchable");
        for excluded in ["Bobby Corner", "Bobby Tackle", "Bobby Edge"] {
            assert!(!body.contains(excluded), "{excluded} must be filtered out");
        }
    }

    /// A search hit is a button that resolves the row in one click — not an
    /// `<option>` swapped into a collapsed `<select>`, which showed the admin
    /// nothing and made the feature look dead.
    #[tokio::test]
    async fn search_results_are_clickable_resolve_buttons() {
        let app = TestApp::new().await;
        app.login_admin().await;
        app.nfl
            .seed_for_test(
                &[player("00-9", "Traded Player", "KC")],
                &[game("2025_01_BUF_NYJ", "BUF", "NYJ")],
            )
            .await
            .unwrap();
        let (import_id, row_id) = stage_one_unmatched(&app).await;

        let resp = app
            .get(&format!(
                "/admin/salary-imports/{import_id}/rows/{row_id}/players?q=Traded&scope=all"
            ))
            .await;
        resp.assert_status_ok();
        let body = resp.text();
        assert!(body.contains("Traded Player"), "search hit is listed");
        assert!(
            body.contains(&format!(
                "hx-post=\"/admin/salary-imports/{import_id}/rows/{row_id}?group=unmatched\""
            )),
            "hit posts back to this row's resolve endpoint, body was:\n{body}"
        );
        assert!(
            body.contains("name=\"gsis_player_id\" value=\"00-9\""),
            "hit submits its gsis id, body was:\n{body}"
        );
        assert!(!body.contains("<option"), "no <select> options any more");
    }

    /// Typing used to swap the week roster for a global list with nothing on screen
    /// marking the change. It now filters what is already there.
    #[tokio::test]
    async fn typing_filters_the_week_roster_in_place() {
        let app = TestApp::new().await;
        app.login_admin().await;
        app.nfl
            .seed_for_test(
                &[player_at("00-9", "Faraway Smith", "WR", "KC")],
                &[game("2025_01_BUF_NYJ", "BUF", "NYJ")],
            )
            .await
            .unwrap();
        app.nfl
            .seed_weekly_roster_for_test(&[
                weekly("00-1", "Buffalo Smith", "WR", "BUF", 1),
                weekly("00-2", "Buffalo Jones", "RB", "BUF", 1),
            ])
            .await
            .unwrap();
        let (import_id, row_id) = stage_one_unmatched(&app).await;

        let body = app
            .get(&format!(
                "/admin/salary-imports/{import_id}/rows/{row_id}/players?q=smith"
            ))
            .await
            .text();
        assert!(body.contains("Buffalo Smith"), "week roster is filtered");
        assert!(!body.contains("Buffalo Jones"), "non-matches drop out");
        assert!(
            !body.contains("Faraway Smith"),
            "other teams stay out by default"
        );
        assert!(
            body.contains("on BUF, week 1"),
            "footer names the scope, body:\n{body}"
        );
        assert!(body.contains("scope=all"), "and offers to widen");
    }

    /// One character is enough for a ~30-name roster.
    #[tokio::test]
    async fn roster_filtering_starts_at_one_character() {
        let app = TestApp::new().await;
        app.login_admin().await;
        app.nfl
            .seed_for_test(&[], &[game("2025_01_BUF_NYJ", "BUF", "NYJ")])
            .await
            .unwrap();
        app.nfl
            .seed_weekly_roster_for_test(&[
                weekly("00-1", "Buffalo Smith", "WR", "BUF", 1),
                weekly("00-2", "Zebra Jones", "RB", "BUF", 1),
            ])
            .await
            .unwrap();
        let (import_id, row_id) = stage_one_unmatched(&app).await;

        let body = app
            .get(&format!(
                "/admin/salary-imports/{import_id}/rows/{row_id}/players?q=z"
            ))
            .await
            .text();
        assert!(body.contains("Zebra Jones"));
        assert!(!body.contains("Buffalo Smith"));
    }

    #[tokio::test]
    async fn scope_all_reaches_a_player_on_another_team() {
        let app = TestApp::new().await;
        app.login_admin().await;
        app.nfl
            .seed_for_test(
                &[player_at("00-9", "Faraway Smith", "WR", "KC")],
                &[game("2025_01_BUF_NYJ", "BUF", "NYJ")],
            )
            .await
            .unwrap();
        app.nfl
            .seed_weekly_roster_for_test(&[weekly("00-1", "Buffalo Jones", "RB", "BUF", 1)])
            .await
            .unwrap();
        let (import_id, row_id) = stage_one_unmatched(&app).await;

        let body = app
            .get(&format!(
                "/admin/salary-imports/{import_id}/rows/{row_id}/players?q=smith&scope=all"
            ))
            .await
            .text();
        assert!(
            body.contains("Faraway Smith"),
            "widened past the week roster"
        );
        assert!(
            body.contains("across all teams and weeks"),
            "footer names the widened scope, body:\n{body}"
        );
        // Already widened: offering the widen again would do nothing, and would
        // suggest there is somewhere further to go.
        assert!(
            !body.contains("Search all teams"),
            "the widen control retires once widened, body:\n{body}"
        );
        assert!(
            !body.contains("scope=all"),
            "no widen request is offered either, body:\n{body}"
        );
    }

    /// The count line sits outside the fragment's emptiness check on purpose: a
    /// filter that matched nothing still has to say what it searched, or the
    /// admin cannot tell a scope with no matches from a scope with no players —
    /// and has nothing on screen telling him widening is the way out.
    #[tokio::test]
    async fn a_filter_that_matches_nothing_still_names_its_scope() {
        let app = TestApp::new().await;
        app.login_admin().await;
        app.nfl
            .seed_for_test(&[], &[game("2025_01_BUF_NYJ", "BUF", "NYJ")])
            .await
            .unwrap();
        app.nfl
            .seed_weekly_roster_for_test(&[weekly("00-1", "Buffalo Smith", "WR", "BUF", 1)])
            .await
            .unwrap();
        let (import_id, row_id) = stage_one_unmatched(&app).await;

        let body = app
            .get(&format!(
                "/admin/salary-imports/{import_id}/rows/{row_id}/players?q=zzz"
            ))
            .await
            .text();
        assert!(body.contains("No players match that search."));
        assert!(
            body.contains("on BUF, week 1"),
            "an empty result still names its scope, body:\n{body}"
        );
        assert!(
            body.contains("scope=all"),
            "and still offers the widen, body:\n{body}"
        );
    }

    /// The count line captions the list that was actually built. When the row's
    /// week has no synced roster the list silently falls back to the player
    /// table's latest-team scoping — a different list, which must not be
    /// captioned as that week's roster.
    #[tokio::test]
    async fn the_latest_team_fallback_does_not_claim_a_week() {
        let app = TestApp::new().await;
        app.login_admin().await;
        // A week-1 game exists, so the week is known — but no weekly roster was
        // ever synced for it, which is what triggers the fallback.
        app.nfl
            .seed_for_test(
                &[player("00-2", "Some Buffalo Guy", "BUF")],
                &[game("2025_01_BUF_NYJ", "BUF", "NYJ")],
            )
            .await
            .unwrap();
        let (import_id, row_id) = stage_one_unmatched(&app).await;

        let body = app
            .get(&format!(
                "/admin/salary-imports/{import_id}/rows/{row_id}/players"
            ))
            .await
            .text();
        assert!(
            body.contains("Some Buffalo Guy"),
            "the fallback list is offered, body:\n{body}"
        );
        assert!(
            !body.contains("week 1"),
            "a latest-team list must not be captioned as week 1, body:\n{body}"
        );
        assert!(
            body.contains("no weekly roster synced"),
            "and says why it is not week-scoped, body:\n{body}"
        );
    }

    /// Regression: every keystroke re-renders this list, so clearing the box must
    /// restore the row's team candidates. Returning an empty list instead wiped
    /// the candidates the row shipped with, and only a page reload brought them
    /// back.
    #[tokio::test]
    async fn empty_query_restores_the_team_candidates() {
        let app = TestApp::new().await;
        app.login_admin().await;
        app.nfl
            .seed_for_test(
                &[
                    player("00-2", "Some Buffalo Guy", "BUF"),
                    player("00-9", "Traded Player", "KC"),
                ],
                &[game("2025_01_BUF_NYJ", "BUF", "NYJ")],
            )
            .await
            .unwrap();
        let (import_id, row_id) = stage_one_unmatched(&app).await;

        let resp = app
            .get(&format!(
                "/admin/salary-imports/{import_id}/rows/{row_id}/players?q="
            ))
            .await;
        resp.assert_status_ok();
        let body = resp.text();
        assert!(
            body.contains("Some Buffalo Guy"),
            "an empty query restores the BUF candidates, body was:\n{body}"
        );
        assert!(
            !body.contains("Traded Player"),
            "an empty query stays team-scoped"
        );
    }

    /// The roster filter starts at one character, but the widened search does not:
    /// `players` holds ~1800 names, so a single letter matches most of the table
    /// and is not a search. The floor belongs to the widened path alone.
    #[tokio::test]
    async fn widened_search_still_needs_two_characters() {
        let app = TestApp::new().await;
        app.login_admin().await;
        app.nfl
            .seed_for_test(
                &[player("00-9", "Traded Player", "KC")],
                &[game("2025_01_BUF_NYJ", "BUF", "NYJ")],
            )
            .await
            .unwrap();
        let (import_id, row_id) = stage_one_unmatched(&app).await;

        let body = app
            .get(&format!(
                "/admin/salary-imports/{import_id}/rows/{row_id}/players?q=T&scope=all"
            ))
            .await
            .text();
        assert!(
            !body.contains("Traded Player"),
            "one character must not dump the player table, body was:\n{body}"
        );
    }

    /// Admins scan these lists by surname. Sorting on `full_name` buries a
    /// player under his first name, and deriving a surname from `full_name`
    /// would sort "Dante Fowler Jr." under "Jr.".
    #[tokio::test]
    async fn team_candidates_are_sorted_by_last_name() {
        let app = TestApp::new().await;
        app.login_admin().await;
        app.nfl
            .seed_for_test(&[], &[game("2025_01_BUF_NYJ", "BUF", "NYJ")])
            .await
            .unwrap();
        app.nfl
            .seed_weekly_roster_for_test(&[
                weekly_named("00-1", "Zach Adams", "Adams", "WR", "BUF", 1),
                weekly_named("00-2", "Adam Zeller", "Zeller", "WR", "BUF", 1),
                weekly_named("00-3", "Dante Fowler Jr.", "Fowler", "RB", "BUF", 1),
            ])
            .await
            .unwrap();
        let (import_id, row_id) = stage_one_unmatched(&app).await;

        let resp = app
            .get(&format!(
                "/admin/salary-imports/{import_id}/rows/{row_id}/players"
            ))
            .await;
        resp.assert_status_ok();
        let body = resp.text();
        let at = |n: &str| body.find(n).unwrap_or_else(|| panic!("{n} missing"));
        assert!(
            at("Zach Adams") < at("Dante Fowler Jr."),
            "Adams before Fowler"
        );
        assert!(
            at("Dante Fowler Jr.") < at("Adam Zeller"),
            "Fowler before Zeller"
        );
    }

    /// The widened search path reads the `players` table, which carries its own
    /// last_name, and must order the same way.
    #[tokio::test]
    async fn search_results_are_sorted_by_last_name() {
        let app = TestApp::new().await;
        app.login_admin().await;
        app.nfl
            .seed_for_test(
                &[
                    player_named("00-1", "Zach Bobbins", "Bobbins", "WR", "KC"),
                    player_named("00-2", "Adam Bobzeller", "Bobzeller", "WR", "KC"),
                    player_named("00-3", "Dante Bobfowler Jr.", "Bobfowler", "RB", "KC"),
                ],
                &[game("2025_01_BUF_NYJ", "BUF", "NYJ")],
            )
            .await
            .unwrap();
        let (import_id, row_id) = stage_one_unmatched(&app).await;

        let resp = app
            .get(&format!(
                "/admin/salary-imports/{import_id}/rows/{row_id}/players?q=bob&scope=all"
            ))
            .await;
        resp.assert_status_ok();
        let body = resp.text();
        let at = |n: &str| body.find(n).unwrap_or_else(|| panic!("{n} missing"));
        assert!(at("Zach Bobbins") < at("Dante Bobfowler Jr."));
        assert!(at("Dante Bobfowler Jr.") < at("Adam Bobzeller"));
    }

    /// The team default can run to hundreds of players, so it is capped — say so
    /// rather than silently truncating.
    #[tokio::test]
    async fn truncated_candidate_list_reports_the_total() {
        let app = TestApp::new().await;
        app.login_admin().await;
        let roster: Vec<Player> = (0..105)
            .map(|i| player(&format!("00-{i}"), &format!("Buffalo Guy {i}"), "BUF"))
            .collect();
        app.nfl
            .seed_for_test(&roster, &[game("2025_01_BUF_NYJ", "BUF", "NYJ")])
            .await
            .unwrap();
        let (import_id, row_id) = stage_one_unmatched(&app).await;

        let resp = app
            .get(&format!(
                "/admin/salary-imports/{import_id}/rows/{row_id}/players"
            ))
            .await;
        resp.assert_status_ok();
        let body = resp.text();
        assert!(
            body.contains("100 of 105"),
            "cap is disclosed, body was:\n{body}"
        );
    }

    #[tokio::test]
    async fn commit_writes_salaries_and_marks_committed() {
        let app = TestApp::new().await;
        app.login_admin().await;
        app.nfl
            .seed_for_test(
                &[player("00-1", "Josh Allen", "BUF")],
                &[game("2025_01_BUF_NYJ", "BUF", "NYJ")],
            )
            .await
            .unwrap();
        let csv = format!(
            "{HDR}\n\"1-100\",\"QB\",\"Josh\",\"\",\"Allen\",\"0\",\"0\",\"8000\",\"BUF@NYJ\",\"BUF\",\"NYJ\",\"\",\"\"\n"
        );
        let form = MultipartForm::new().add_part(
            "csv",
            Part::bytes(csv.into_bytes())
                .file_name("s.csv")
                .mime_type("text/csv"),
        );
        let loc = app
            .server
            .post("/admin/salary-imports")
            .multipart(form)
            .await
            .header("location")
            .to_str()
            .unwrap()
            .to_string();
        let import_id: i64 = loc.rsplit('/').next().unwrap().parse().unwrap();

        let resp = app
            .post(&format!("/admin/salary-imports/{import_id}/commit"), "")
            .await;
        resp.assert_status(StatusCode::SEE_OTHER);
        assert_eq!(
            import_store::get(&app.pool, import_id)
                .await
                .unwrap()
                .unwrap()
                .status,
            "committed"
        );
        let salaries = crate::player_salaries::store::for_games(&app.pool, &["2025_01_BUF_NYJ"])
            .await
            .unwrap();
        assert_eq!(salaries.len(), 1);

        // The success flash survives the redirect: following it renders the
        // toast on the next page.
        let page = app.get(&loc).await;
        page.assert_text_contains("Imported 1 salaries.");
        page.assert_text_contains("alert-success");
    }

    #[tokio::test]
    async fn discard_deletes_pending_batch() {
        let app = TestApp::new().await;
        app.login_admin().await;
        app.nfl
            .seed_for_test(
                &[player("00-1", "Josh Allen", "BUF")],
                &[game("2025_01_BUF_NYJ", "BUF", "NYJ")],
            )
            .await
            .unwrap();
        let csv = format!(
            "{HDR}\n\"1-100\",\"QB\",\"Josh\",\"\",\"Allen\",\"0\",\"0\",\"8000\",\"BUF@NYJ\",\"BUF\",\"NYJ\",\"\",\"\"\n"
        );
        let form = MultipartForm::new().add_part(
            "csv",
            Part::bytes(csv.into_bytes())
                .file_name("s.csv")
                .mime_type("text/csv"),
        );
        let loc = app
            .server
            .post("/admin/salary-imports")
            .multipart(form)
            .await
            .header("location")
            .to_str()
            .unwrap()
            .to_string();
        let import_id: i64 = loc.rsplit('/').next().unwrap().parse().unwrap();

        let resp = app
            .post(&format!("/admin/salary-imports/{import_id}/discard"), "")
            .await;
        resp.assert_status(StatusCode::SEE_OTHER);
        assert!(
            import_store::get(&app.pool, import_id)
                .await
                .unwrap()
                .is_none()
        );
    }

    /// 643 of import 1's 724 rows are auto-matched. Rendering them cost 238 KB and
    /// gave the admin nothing to do.
    #[tokio::test]
    async fn review_page_omits_auto_matched_rows_and_counts_them() {
        let app = TestApp::new().await;
        app.login_admin().await;
        app.nfl
            .seed_for_test(
                &[player("00-1", "Real Player", "BUF")],
                &[game("2025_01_BUF_NYJ", "BUF", "NYJ")],
            )
            .await
            .unwrap();
        // Two CSV rows: one matches "Real Player" by name, one does not.
        let csv = format!(
            "{HDR}\n\"1-1\",\"WR\",\"Real\",\"\",\"Player\",\"0\",\"0\",\"5000\",\"BUF@NYJ\",\"BUF\",\"NYJ\",\"\",\"\"\n\"1-2\",\"WR\",\"Nobody\",\"\",\"Here\",\"0\",\"0\",\"4000\",\"BUF@NYJ\",\"BUF\",\"NYJ\",\"\",\"\"\n"
        );
        let form = MultipartForm::new().add_part(
            "csv",
            Part::bytes(csv.into_bytes())
                .file_name("s.csv")
                .mime_type("text/csv"),
        );
        let loc = app
            .server
            .post("/admin/salary-imports")
            .multipart(form)
            .await
            .header("location")
            .to_str()
            .unwrap()
            .to_string();

        let import_id: i64 = loc.rsplit('/').next().unwrap().parse().unwrap();
        // By id, not by name: the assertion has to survive the row markup being
        // restyled, and "the name is absent" would pass for the wrong reason the
        // day that `<span>` becomes a `<div>`.
        let matched_id = import_store::rows_for(&app.pool, import_id)
            .await
            .unwrap()
            .into_iter()
            .find(|r| r.state == RowState::Matched)
            .expect("the Real Player row auto-matched")
            .id;

        let body = app.get(&loc).await.text();
        assert!(body.contains("Nobody Here"), "the unmatched row is shown");
        assert!(
            !body.contains(&format!("id=\"row-{matched_id}\"")),
            "the matched row is not rendered, body:\n{body}"
        );
        // The counts themselves, not just the word: a header that says
        // "0 auto-matched" is exactly the bug this test exists to catch.
        assert!(
            body.contains("1 auto-matched"),
            "the matched row is counted, body:\n{body}"
        );
        assert!(
            body.contains("(1 matched, 0 D/ST)"),
            "the header breaks the count down by state, body:\n{body}"
        );
    }

    #[tokio::test]
    async fn triage_defaults_to_unmatched_and_bad_group_falls_back() {
        let app = TestApp::new().await;
        app.login_admin().await;
        app.nfl
            .seed_for_test(&[], &[game("2025_01_BUF_NYJ", "BUF", "NYJ")])
            .await
            .unwrap();
        let (import_id, _) = stage_one_unmatched(&app).await;

        for query in ["", "?group=nonsense", "?group=unmatched"] {
            let resp = app
                .get(&format!("/admin/salary-imports/{import_id}/triage{query}"))
                .await;
            resp.assert_status_ok();
            let body = resp.text();
            assert!(
                body.contains("Nobody Here"),
                "{query} shows the unmatched group"
            );
            assert!(
                body.contains("Needs a decision"),
                "{query} labels the group"
            );
        }
    }

    #[tokio::test]
    async fn resolved_group_names_the_player_that_was_picked() {
        let app = TestApp::new().await;
        app.login_admin().await;
        app.nfl
            .seed_for_test(
                &[player_at("00-7", "Picked Guy", "TE", "BUF")],
                &[game("2025_01_BUF_NYJ", "BUF", "NYJ")],
            )
            .await
            .unwrap();
        let (import_id, row_id) = stage_one_unmatched(&app).await;
        app.post_htmx(
            &format!("/admin/salary-imports/{import_id}/rows/{row_id}"),
            "gsis_player_id=00-7",
        )
        .await;

        let body = app
            .get(&format!(
                "/admin/salary-imports/{import_id}/triage?group=resolved"
            ))
            .await
            .text();
        assert!(
            body.contains("Picked Guy"),
            "resolved row names its player, body:\n{body}"
        );
        assert!(body.contains("Undo"), "resolved row offers undo");
    }

    /// The season is identical on every row of the listing and identifies
    /// nothing; the week is what an admin recognises an import by.
    #[tokio::test]
    async fn recent_imports_are_listed_by_week() {
        let app = TestApp::new().await;
        app.login_admin().await;
        app.nfl
            .seed_for_test(&[], &[game("2025_01_BUF_NYJ", "BUF", "NYJ")])
            .await
            .unwrap();
        stage_one_unmatched(&app).await;

        let body = app.get("/admin/salary-imports").await.text();
        assert!(
            body.contains("week 1"),
            "listing names the week, body:\n{body}"
        );
        assert!(
            !body.contains("season 2025"),
            "and not the season, which is the same on every row"
        );
    }

    /// A committed import is a record, not a worklist: no decisions remain, so
    /// offering Commit, Discard, Skip or Undo on it is wrong.
    ///
    /// It lists only the rows a human touched. The auto-matched ones are the
    /// bulk — 699 of import 1's 707 — and reading them tells the admin nothing
    /// they decided.
    #[tokio::test]
    async fn a_committed_import_shows_what_landed_instead_of_the_triage_ui() {
        let app = TestApp::new().await;
        app.login_admin().await;
        app.nfl
            .seed_for_test(
                &[
                    player_at("00-7", "Real Player", "WR", "BUF"),
                    player_at("00-8", "Hand Picked", "WR", "BUF"),
                ],
                &[game("2025_01_BUF_NYJ", "BUF", "NYJ")],
            )
            .await
            .unwrap();
        // Three rows: one auto-matches, one gets resolved by hand, one skipped.
        let csv = format!(
            "{HDR}\n\
             \"1-1\",\"WR\",\"Real\",\"\",\"Player\",\"0\",\"0\",\"5000\",\"BUF@NYJ\",\"BUF\",\"NYJ\",\"\",\"\"\n\
             \"1-2\",\"WR\",\"Nobody\",\"\",\"Here\",\"0\",\"0\",\"4000\",\"BUF@NYJ\",\"BUF\",\"NYJ\",\"\",\"\"\n\
             \"1-3\",\"WR\",\"Cryptic\",\"\",\"Alias\",\"0\",\"0\",\"4500\",\"BUF@NYJ\",\"BUF\",\"NYJ\",\"\",\"\"\n"
        );
        let form = MultipartForm::new().add_part(
            "csv",
            Part::bytes(csv.into_bytes())
                .file_name("s.csv")
                .mime_type("text/csv"),
        );
        let loc = app
            .server
            .post("/admin/salary-imports")
            .multipart(form)
            .await
            .header("location")
            .to_str()
            .unwrap()
            .to_string();
        let import_id: i64 = loc.rsplit('/').next().unwrap().parse().unwrap();
        let unmatched: Vec<(i64, String)> = import_store::rows_for(&app.pool, import_id)
            .await
            .unwrap()
            .into_iter()
            .filter(|r| r.state == RowState::Unmatched)
            .map(|r| (r.id, r.fd_name))
            .collect();
        for (row_id, name) in &unmatched {
            let body = if name == "Cryptic Alias" {
                "gsis_player_id=00-8"
            } else {
                "skip=1"
            };
            app.post_htmx(
                &format!("/admin/salary-imports/{import_id}/rows/{row_id}"),
                body,
            )
            .await;
        }
        app.post(&format!("/admin/salary-imports/{import_id}/commit"), "")
            .await;

        let body = app.get(&loc).await.text();
        assert!(body.contains("Week 1"), "names the week, body:\n{body}");
        assert!(
            body.contains("2 salaries imported"),
            "counts what landed, body:\n{body}"
        );
        assert!(
            body.contains("Cryptic Alias"),
            "lists the row resolved by hand, body:\n{body}"
        );
        assert!(
            body.contains("Hand Picked"),
            "and names who it was resolved to, body:\n{body}"
        );
        assert!(
            body.contains("Nobody Here"),
            "lists what was deliberately left out, body:\n{body}"
        );
        assert!(
            !body.contains("Real Player"),
            "but not the auto-matched rows, body:\n{body}"
        );
        // The app shell carries its own hx-post (Sign out), so check only for
        // posts back into this import — a decision, commit, or discard.
        for gone in [
            "Commit salaries",
            "Needs a decision",
            &format!("hx-post=\"/admin/salary-imports/{import_id}"),
            &format!("action=\"/admin/salary-imports/{import_id}"),
        ] {
            assert!(
                !body.contains(gone),
                "a committed import must not offer {gone:?}, body:\n{body}"
            );
        }
    }

    /// Each group is its own address, so a tab can be linked, bookmarked, and
    /// reloaded. The default group stays on the bare import URL.
    #[tokio::test]
    async fn every_group_has_its_own_url() {
        let app = TestApp::new().await;
        app.login_admin().await;
        app.nfl
            .seed_for_test(&[], &[game("2025_01_BUF_NYJ", "BUF", "NYJ")])
            .await
            .unwrap();
        let (import_id, row_id) = stage_one_unmatched(&app).await;
        app.post_htmx(
            &format!("/admin/salary-imports/{import_id}/rows/{row_id}"),
            "skip=1",
        )
        .await;

        // (path, the tab that must come back active)
        for (path, active) in [
            (
                format!("/admin/salary-imports/{import_id}"),
                "Needs a decision",
            ),
            (
                format!("/admin/salary-imports/{import_id}/resolved"),
                "Resolved",
            ),
            (
                format!("/admin/salary-imports/{import_id}/skipped"),
                "Skipped",
            ),
        ] {
            let resp = app.get(&path).await;
            resp.assert_status_ok();
            let body = resp.text();
            assert!(
                body.contains("<!doctype html>"),
                "{path} is a whole page, not a fragment"
            );
            let active_tab = body
                .split("tab tab-active")
                .nth(1)
                .unwrap_or_default()
                .split("</button>")
                .next()
                .unwrap_or_default();
            assert!(
                active_tab.contains(active),
                "{path} should open on {active}, got:\n{active_tab}"
            );
        }
        // The skipped row is really there, so the assertion above is not just
        // reading an empty tab.
        assert!(
            app.get(&format!("/admin/salary-imports/{import_id}/skipped"))
                .await
                .text()
                .contains("Nobody Here"),
            "the skipped group lists its row"
        );
    }

    /// Clicking a tab has to leave the address bar pointing at what is on
    /// screen, or a reload silently throws the admin back to the first group.
    #[tokio::test]
    async fn tabs_push_their_own_url() {
        let app = TestApp::new().await;
        app.login_admin().await;
        app.nfl
            .seed_for_test(&[], &[game("2025_01_BUF_NYJ", "BUF", "NYJ")])
            .await
            .unwrap();
        let (import_id, _) = stage_one_unmatched(&app).await;

        let body = app
            .get(&format!("/admin/salary-imports/{import_id}"))
            .await
            .text();
        for expected in [
            format!("hx-push-url=\"/admin/salary-imports/{import_id}\""),
            format!("hx-push-url=\"/admin/salary-imports/{import_id}/resolved\""),
            format!("hx-push-url=\"/admin/salary-imports/{import_id}/skipped\""),
        ] {
            assert!(
                body.contains(&expected),
                "missing {expected}, body:\n{body}"
            );
        }
    }

    /// A likely-but-unproven match is offered as one click, above the search
    /// box. It must name the player, not just its id, or there is nothing to
    /// judge — and it must not be pre-applied.
    #[tokio::test]
    async fn an_unmatched_row_offers_its_suggestion_for_confirmation() {
        let app = TestApp::new().await;
        app.login_admin().await;
        app.nfl
            .seed_for_test(&[], &[game("2025_01_BUF_NYJ", "BUF", "NYJ")])
            .await
            .unwrap();
        // `stage_one_unmatched` stages "Nobody Here", a WR on BUF. This roster
        // entry shares the surname and position but not the first name — the
        // nickname shape ("Zonovan Knight" is rostered as "Bam Knight").
        app.nfl
            .seed_weekly_roster_for_test(&[weekly("00-bam", "Somebody Here", "WR", "BUF", 1)])
            .await
            .unwrap();
        let (import_id, row_id) = stage_one_unmatched(&app).await;

        let body = app
            .get(&format!("/admin/salary-imports/{import_id}"))
            .await
            .text();
        assert!(
            body.contains("Looks like Somebody Here (WR)"),
            "the suggestion names the player, body:\n{body}"
        );
        assert!(body.contains("Confirm"), "and offers one click to take it");
        assert!(
            body.contains(&format!(
                "/admin/salary-imports/{import_id}/rows/{row_id}?group=unmatched"
            )),
            "confirming posts the normal resolve, body:\n{body}"
        );
        assert_eq!(
            import_store::unmatched_count(&app.pool, import_id)
                .await
                .unwrap(),
            1,
            "and nothing was decided without the admin"
        );
    }

    /// No suggestion, no affordance — an empty "Looks like" would be noise on
    /// every row the matcher had no opinion about.
    #[tokio::test]
    async fn a_row_without_a_suggestion_shows_no_confirm() {
        let app = TestApp::new().await;
        app.login_admin().await;
        app.nfl
            .seed_for_test(&[], &[game("2025_01_BUF_NYJ", "BUF", "NYJ")])
            .await
            .unwrap();
        let (import_id, _) = stage_one_unmatched(&app).await;

        let body = app
            .get(&format!("/admin/salary-imports/{import_id}"))
            .await
            .text();
        assert!(!body.contains("Confirm"), "body:\n{body}");
    }

    /// Candidates are drawn from the weekly roster, but 144 of 2025's 3,133
    /// rostered players are absent from the `players` table — and those are
    /// exactly the ones an admin resolves by hand, because `plan()` matches
    /// against `players`, so a player missing from it can never auto-match.
    /// Naming the pick from `players` alone therefore fails on the common case.
    #[tokio::test]
    async fn a_row_resolved_to_a_roster_only_player_is_named() {
        let app = TestApp::new().await;
        app.login_admin().await;
        app.nfl
            .seed_for_test(&[], &[game("2025_01_BUF_NYJ", "BUF", "NYJ")])
            .await
            .unwrap();
        // On the roster the admin picked from, absent from the player table.
        app.nfl
            .seed_weekly_roster_for_test(&[weekly("00-4040", "Roster Only Guy", "WR", "BUF", 1)])
            .await
            .unwrap();
        let (import_id, row_id) = stage_one_unmatched(&app).await;
        app.post_htmx(
            &format!("/admin/salary-imports/{import_id}/rows/{row_id}"),
            "gsis_player_id=00-4040",
        )
        .await;

        let body = app
            .get(&format!(
                "/admin/salary-imports/{import_id}/triage?group=resolved"
            ))
            .await
            .text();
        assert!(
            body.contains("Roster Only Guy"),
            "the pick is named from the roster it was picked from, body:\n{body}"
        );
        assert!(
            !body.contains("→ 00-4040"),
            "and is not reduced to its raw id, body:\n{body}"
        );
    }

    /// A pick neither source has heard of still has to say *something*.
    /// Falling back to nothing renders a bare "→" and loses the only record of
    /// what the admin chose.
    #[tokio::test]
    async fn a_resolved_row_falls_back_to_the_raw_gsis_id() {
        let app = TestApp::new().await;
        app.login_admin().await;
        app.nfl
            .seed_for_test(&[], &[game("2025_01_BUF_NYJ", "BUF", "NYJ")])
            .await
            .unwrap();
        let (import_id, row_id) = stage_one_unmatched(&app).await;
        // Nothing was seeded under this id, so `players_by_gsis` returns a map
        // with no entry for it.
        app.post_htmx(
            &format!("/admin/salary-imports/{import_id}/rows/{row_id}"),
            "gsis_player_id=00-9999999",
        )
        .await;

        let body = app
            .get(&format!(
                "/admin/salary-imports/{import_id}/triage?group=resolved"
            ))
            .await
            .text();
        assert!(
            body.contains("→ 00-9999999"),
            "an unknown pick shows its raw gsis id, body:\n{body}"
        );
    }

    /// Both entry points 404 on a batch that does not exist. Pinned because
    /// `build_triage` signals "no such batch" as `Ok(None)`, which is one
    /// careless `unwrap_or_default` away from rendering an empty page instead.
    #[tokio::test]
    async fn a_missing_import_is_not_found() {
        let app = TestApp::new().await;
        app.login_admin().await;

        for uri in [
            "/admin/salary-imports/999999",
            "/admin/salary-imports/999999/triage",
        ] {
            app.get(uri).await.assert_status(StatusCode::NOT_FOUND);
        }
    }

    #[tokio::test]
    async fn an_empty_group_says_so() {
        let app = TestApp::new().await;
        app.login_admin().await;
        app.nfl
            .seed_for_test(&[], &[game("2025_01_BUF_NYJ", "BUF", "NYJ")])
            .await
            .unwrap();
        let (import_id, _) = stage_one_unmatched(&app).await;

        let body = app
            .get(&format!(
                "/admin/salary-imports/{import_id}/triage?group=skipped"
            ))
            .await
            .text();
        assert!(
            body.contains("No rows skipped"),
            "empty state, body:\n{body}"
        );
    }

    /// A decision moves the row out of the group on screen, so the response is the
    /// whole group, not the row.
    #[tokio::test]
    async fn resolving_returns_the_triage_fragment_without_the_row() {
        let app = TestApp::new().await;
        app.login_admin().await;
        app.nfl
            .seed_for_test(
                &[player_at("00-7", "Picked Guy", "TE", "BUF")],
                &[game("2025_01_BUF_NYJ", "BUF", "NYJ")],
            )
            .await
            .unwrap();
        let (import_id, row_id) = stage_one_unmatched(&app).await;

        let resp = app
            .post_htmx(
                &format!("/admin/salary-imports/{import_id}/rows/{row_id}?group=unmatched"),
                "gsis_player_id=00-7",
            )
            .await;
        resp.assert_status_ok();
        let body = resp.text();
        assert!(
            body.contains("id=\"triage\""),
            "response is the triage fragment"
        );
        assert!(
            !body.contains("Nobody Here"),
            "row left the unmatched group"
        );
        assert!(
            body.contains("Every row has a decision."),
            "empty state rendered"
        );
    }

    /// Picking a candidate is how most rows get resolved, so that form has to
    /// target the triage fragment too. Left targeting `#row-{id}`, the response
    /// swaps a whole group — tabs and all — into a single `<li>`.
    #[tokio::test]
    async fn picking_a_candidate_returns_the_triage_fragment() {
        let app = TestApp::new().await;
        app.login_admin().await;
        app.nfl
            .seed_for_test(
                &[player_at("00-7", "Picked Guy", "TE", "BUF")],
                &[game("2025_01_BUF_NYJ", "BUF", "NYJ")],
            )
            .await
            .unwrap();
        let (import_id, row_id) = stage_one_unmatched(&app).await;

        let markup = app
            .get(&format!(
                "/admin/salary-imports/{import_id}/rows/{row_id}/players"
            ))
            .await
            .text();
        assert!(
            markup.contains(&format!(
                "hx-post=\"/admin/salary-imports/{import_id}/rows/{row_id}?group=unmatched\""
            )),
            "candidate form names the group it is deciding from, body was:\n{markup}"
        );
        assert!(
            markup.contains("hx-target=\"#triage\""),
            "candidate form swaps the whole group, body was:\n{markup}"
        );

        let resp = app
            .post_htmx(
                &format!("/admin/salary-imports/{import_id}/rows/{row_id}?group=unmatched"),
                "gsis_player_id=00-7",
            )
            .await;
        resp.assert_status_ok();
        let body = resp.text();
        assert!(
            body.contains("id=\"triage\""),
            "the picked-candidate path returns a triage fragment, body was:\n{body}"
        );
    }

    #[tokio::test]
    async fn undo_returns_a_resolved_row_to_the_unmatched_group() {
        let app = TestApp::new().await;
        app.login_admin().await;
        app.nfl
            .seed_for_test(
                &[player_at("00-7", "Picked Guy", "TE", "BUF")],
                &[game("2025_01_BUF_NYJ", "BUF", "NYJ")],
            )
            .await
            .unwrap();
        let (import_id, row_id) = stage_one_unmatched(&app).await;
        app.post_htmx(
            &format!("/admin/salary-imports/{import_id}/rows/{row_id}"),
            "gsis_player_id=00-7",
        )
        .await;

        let resp = app
            .post_htmx(
                &format!("/admin/salary-imports/{import_id}/rows/{row_id}/undo?group=resolved"),
                "",
            )
            .await;
        resp.assert_status_ok();
        assert!(
            resp.text().contains("No rows resolved yet."),
            "row left the resolved group"
        );
        assert_eq!(
            import_store::unmatched_count(&app.pool, import_id)
                .await
                .unwrap(),
            1,
            "row is actionable again"
        );
    }

    #[tokio::test]
    async fn undo_returns_a_skipped_row_to_the_unmatched_group() {
        let app = TestApp::new().await;
        app.login_admin().await;
        app.nfl
            .seed_for_test(&[], &[game("2025_01_BUF_NYJ", "BUF", "NYJ")])
            .await
            .unwrap();
        let (import_id, row_id) = stage_one_unmatched(&app).await;
        app.post_htmx(
            &format!("/admin/salary-imports/{import_id}/rows/{row_id}"),
            "skip=1",
        )
        .await;

        app.post_htmx(
            &format!("/admin/salary-imports/{import_id}/rows/{row_id}/undo?group=skipped"),
            "",
        )
        .await
        .assert_status_ok();
        assert_eq!(
            import_store::unmatched_count(&app.pool, import_id)
                .await
                .unwrap(),
            1
        );
    }

    /// The `<button>` carrying the commit control, so "is it disabled" is asked
    /// of that element rather than of the whole page.
    fn commit_button(body: &str) -> &str {
        let label = body
            .find("Commit salaries")
            .unwrap_or_else(|| panic!("no commit button in:\n{body}"));
        let open = body[..label].rfind("<button").expect("its opening tag");
        &body[open..label]
    }

    /// `(group slug, marked active, badge count)` for every tab of a rendered
    /// triage fragment.
    fn tabs_in(body: &str) -> Vec<(String, bool, i64)> {
        body.split("role=\"tab\"")
            .skip(1)
            .map(|chunk| {
                let head = &chunk[..chunk.find("</button>").expect("a tab closes")];
                let slug = head
                    .split("?group=")
                    .nth(1)
                    .expect("a tab names its group")
                    .split('"')
                    .next()
                    .expect("slug")
                    .to_string();
                let badge = head.split("<span").nth(1).expect("a tab carries a badge");
                let count = badge[badge.find('>').expect("badge opens") + 1..]
                    .split('<')
                    .next()
                    .expect("badge text")
                    .trim()
                    .parse()
                    .expect("badge count");
                (slug, head.contains("tab-active"), count)
            })
            .collect()
    }

    /// Upload a two-row CSV — one row the importer auto-matches, one it cannot —
    /// and return `(import_id, unmatched_row_id)`.
    async fn stage_one_matched_one_unmatched(app: &TestApp) -> (i64, i64) {
        let csv = format!(
            "{HDR}\n\"1-1\",\"WR\",\"Real\",\"\",\"Player\",\"0\",\"0\",\"5000\",\"BUF@NYJ\",\"BUF\",\"NYJ\",\"\",\"\"\n\"1-2\",\"WR\",\"Nobody\",\"\",\"Here\",\"0\",\"0\",\"4000\",\"BUF@NYJ\",\"BUF\",\"NYJ\",\"\",\"\"\n"
        );
        let form = MultipartForm::new().add_part(
            "csv",
            Part::bytes(csv.into_bytes())
                .file_name("s.csv")
                .mime_type("text/csv"),
        );
        let loc = app
            .server
            .post("/admin/salary-imports")
            .multipart(form)
            .await
            .header("location")
            .to_str()
            .unwrap()
            .to_string();
        let import_id: i64 = loc.rsplit('/').next().unwrap().parse().unwrap();
        let row_id = import_store::rows_for(&app.pool, import_id)
            .await
            .unwrap()
            .into_iter()
            .find(|r| r.state == RowState::Unmatched)
            .expect("one row needs a decision")
            .id;
        (import_id, row_id)
    }

    /// The counts and the commit control live *inside* the swapped fragment, so
    /// the swap that empties the unmatched group is the same swap that enables
    /// Commit. Outside it, resolving the last row rendered "Every row has a
    /// decision." beside a Commit button that stayed disabled until a reload,
    /// with nothing on screen saying a reload was needed.
    #[tokio::test]
    async fn resolving_the_last_row_re_enables_commit_in_the_same_swap() {
        let app = TestApp::new().await;
        app.login_admin().await;
        app.nfl
            .seed_for_test(
                &[
                    player("00-1", "Real Player", "BUF"),
                    player_at("00-7", "Picked Guy", "TE", "BUF"),
                ],
                &[game("2025_01_BUF_NYJ", "BUF", "NYJ")],
            )
            .await
            .unwrap();
        let (import_id, row_id) = stage_one_matched_one_unmatched(&app).await;

        let before = app
            .get(&format!("/admin/salary-imports/{import_id}"))
            .await
            .text();
        assert!(
            commit_button(&before).contains("disabled"),
            "a row still needs a decision, body:\n{before}"
        );

        let resp = app
            .post_htmx(
                &format!("/admin/salary-imports/{import_id}/rows/{row_id}?group=unmatched"),
                "gsis_player_id=00-7",
            )
            .await;
        resp.assert_status_ok();
        let body = resp.text();
        assert!(
            !commit_button(&body).contains("disabled"),
            "the last decision enables Commit in the same swap, body:\n{body}"
        );
        assert!(
            body.contains("1 auto-matched"),
            "and the swap refreshes the header counts, body:\n{body}"
        );
        let tabs = tabs_in(&body);
        assert_eq!(
            tabs.iter().find(|t| t.0 == "unmatched").expect("tab").2,
            0,
            "the unmatched badge is refreshed too, body:\n{body}"
        );
        assert_eq!(
            tabs.iter().find(|t| t.0 == "resolved").expect("tab").2,
            1,
            "and the row is counted where it landed, body:\n{body}"
        );
    }

    /// Commit and Discard are plain full-page posts. Moving them inside the
    /// htmx-swapped fragment must not turn them into htmx requests, or the admin
    /// gets a redirect body swapped into the page instead of a page.
    #[tokio::test]
    async fn commit_and_discard_stay_plain_form_posts() {
        let app = TestApp::new().await;
        app.login_admin().await;
        app.nfl
            .seed_for_test(&[], &[game("2025_01_BUF_NYJ", "BUF", "NYJ")])
            .await
            .unwrap();
        let (import_id, _) = stage_one_unmatched(&app).await;

        let body = app
            .get(&format!("/admin/salary-imports/{import_id}"))
            .await
            .text();
        for action in ["commit", "discard"] {
            assert!(
                body.contains(&format!(
                    "<form method=\"post\" action=\"/admin/salary-imports/{import_id}/{action}\">"
                )),
                "{action} is a plain form post, body:\n{body}"
            );
        }
    }

    /// Typing filters this row's team-and-week roster and nothing else, so the
    /// box must not promise a search of every player: an admin who typed a
    /// traded player's name, saw nothing, and concluded the player was missing
    /// from the system is exactly the failure this wording caused.
    #[tokio::test]
    async fn the_search_box_says_what_it_actually_filters() {
        let app = TestApp::new().await;
        app.login_admin().await;
        app.nfl
            .seed_for_test(&[], &[game("2025_01_BUF_NYJ", "BUF", "NYJ")])
            .await
            .unwrap();
        let (import_id, _) = stage_one_unmatched(&app).await;

        let body = app
            .get(&format!("/admin/salary-imports/{import_id}"))
            .await
            .text();
        assert!(
            !body.contains("search all players"),
            "the box no longer claims a global search, body:\n{body}"
        );
        assert!(
            body.contains("placeholder=\"filter this week's BUF roster…\""),
            "it names the roster it filters, body:\n{body}"
        );
    }

    /// The natural first click on a row whose player is not on the roster:
    /// focus it (empty `q`), then widen. The widened branch has nothing to
    /// search, and used to answer "No players match that search." over
    /// "0 of 0 across all teams and weeks" — with the week roster gone, the
    /// widen control retired, and no way back.
    #[tokio::test]
    async fn widening_with_too_short_a_query_says_so_and_offers_the_way_back() {
        let app = TestApp::new().await;
        app.login_admin().await;
        app.nfl
            .seed_for_test(
                &[player_at("00-9", "Faraway Smith", "WR", "KC")],
                &[game("2025_01_BUF_NYJ", "BUF", "NYJ")],
            )
            .await
            .unwrap();
        app.nfl
            .seed_weekly_roster_for_test(&[weekly("00-1", "Roster Guy", "WR", "BUF", 1)])
            .await
            .unwrap();
        let (import_id, row_id) = stage_one_unmatched(&app).await;
        let players = format!("/admin/salary-imports/{import_id}/rows/{row_id}/players");

        let body = app.get(&format!("{players}?scope=all")).await.text();
        assert!(
            !body.contains("No players match that search."),
            "nothing was searched, so nothing failed to match, body:\n{body}"
        );
        assert!(
            !body.contains("0 of 0"),
            "and no count line claims an empty result, body:\n{body}"
        );
        assert!(
            body.contains("at least two characters"),
            "it says what the admin has to do, body:\n{body}"
        );

        // The way back out of the widened state, on both the empty and the
        // populated widened response.
        let widened_hit = app
            .get(&format!("{players}?q=smith&scope=all"))
            .await
            .text();
        for (label, markup) in [("too short", &body), ("with hits", &widened_hit)] {
            assert!(
                markup.contains(&format!("hx-get=\"{players}\"")),
                "{label}: offers a way back to this row's roster, body:\n{markup}"
            );
            assert!(
                markup.contains("hx-include=\"closest li\""),
                "{label}: carrying the typed text, body:\n{markup}"
            );
            assert!(
                markup.contains(&format!("hx-target=\"#row-{row_id}-results\"")),
                "{label}: into the row's own results container, body:\n{markup}"
            );
            assert!(
                markup.contains("hx-swap=\"innerHTML\""),
                "{label}: as an innerHTML swap, body:\n{markup}"
            );
        }

        // And following it does return the week roster.
        let back = app.get(&players).await.text();
        assert!(
            back.contains("Roster Guy"),
            "the way back restores the week roster, body:\n{back}"
        );
    }

    /// The tab strip is the only thing on screen saying which group is being
    /// shown and how much work is left in the other two. Both are template
    /// details that nothing else asserts.
    #[tokio::test]
    async fn exactly_one_tab_is_active_and_the_badges_count_the_batch() {
        let app = TestApp::new().await;
        app.login_admin().await;
        app.nfl
            .seed_for_test(
                &[
                    player("00-1", "Real Player", "BUF"),
                    player_at("00-7", "Picked Guy", "TE", "BUF"),
                ],
                &[game("2025_01_BUF_NYJ", "BUF", "NYJ")],
            )
            .await
            .unwrap();
        // Three rows needing a decision, of which one is resolved and one
        // skipped, plus one the importer matched on its own.
        let csv = format!(
            "{HDR}\n\"1-1\",\"WR\",\"Real\",\"\",\"Player\",\"0\",\"0\",\"5000\",\"BUF@NYJ\",\"BUF\",\"NYJ\",\"\",\"\"\n\"1-2\",\"WR\",\"Nobody\",\"\",\"One\",\"0\",\"0\",\"4000\",\"BUF@NYJ\",\"BUF\",\"NYJ\",\"\",\"\"\n\"1-3\",\"WR\",\"Nobody\",\"\",\"Two\",\"0\",\"0\",\"4000\",\"BUF@NYJ\",\"BUF\",\"NYJ\",\"\",\"\"\n\"1-4\",\"WR\",\"Nobody\",\"\",\"Three\",\"0\",\"0\",\"4000\",\"BUF@NYJ\",\"BUF\",\"NYJ\",\"\",\"\"\n"
        );
        let form = MultipartForm::new().add_part(
            "csv",
            Part::bytes(csv.into_bytes())
                .file_name("s.csv")
                .mime_type("text/csv"),
        );
        let loc = app
            .server
            .post("/admin/salary-imports")
            .multipart(form)
            .await
            .header("location")
            .to_str()
            .unwrap()
            .to_string();
        let import_id: i64 = loc.rsplit('/').next().unwrap().parse().unwrap();
        let undecided: Vec<i64> = import_store::rows_for(&app.pool, import_id)
            .await
            .unwrap()
            .into_iter()
            .filter(|r| r.state == RowState::Unmatched)
            .map(|r| r.id)
            .collect();
        assert_eq!(undecided.len(), 3);
        app.post_htmx(
            &format!("/admin/salary-imports/{import_id}/rows/{}", undecided[0]),
            "gsis_player_id=00-7",
        )
        .await;
        app.post_htmx(
            &format!("/admin/salary-imports/{import_id}/rows/{}", undecided[1]),
            "skip=1",
        )
        .await;

        let counts = import_store::state_counts(&app.pool, import_id)
            .await
            .unwrap();
        let expected = [
            ("unmatched", counts.unmatched),
            ("resolved", counts.resolved),
            ("skipped", counts.skipped),
        ];
        assert_eq!(
            expected,
            [("unmatched", 1), ("resolved", 1), ("skipped", 1)]
        );

        for (requested, _) in expected {
            let body = app
                .get(&format!(
                    "/admin/salary-imports/{import_id}/triage?group={requested}"
                ))
                .await
                .text();
            let tabs = tabs_in(&body);
            assert_eq!(tabs.len(), 3, "one tab per group, body:\n{body}");
            let active: Vec<&String> = tabs.iter().filter(|t| t.1).map(|t| &t.0).collect();
            assert_eq!(
                active,
                vec![&requested.to_string()],
                "exactly one tab is active, and it is the requested group, body:\n{body}"
            );
            for (slug, count) in expected {
                assert_eq!(
                    tabs.iter().find(|t| t.0 == slug).expect("tab").2,
                    count,
                    "the {slug} badge counts that group, body:\n{body}"
                );
            }
        }
    }

    /// Skip and Undo are decisions, so they swap the whole group like every
    /// other decision path — and return the admin to the tab he was on. Reached
    /// in every other test by hand-built URLs, so nothing pinned the markup that
    /// the browser actually posts.
    #[tokio::test]
    async fn the_skip_and_undo_forms_swap_the_whole_group() {
        let app = TestApp::new().await;
        app.login_admin().await;
        app.nfl
            .seed_for_test(
                &[player_at("00-7", "Picked Guy", "TE", "BUF")],
                &[game("2025_01_BUF_NYJ", "BUF", "NYJ")],
            )
            .await
            .unwrap();
        let (import_id, row_id) = stage_one_unmatched(&app).await;

        let unmatched = app
            .get(&format!(
                "/admin/salary-imports/{import_id}/triage?group=unmatched"
            ))
            .await
            .text();
        assert!(
            unmatched.contains(&format!(
                "hx-post=\"/admin/salary-imports/{import_id}/rows/{row_id}?group=unmatched\""
            )),
            "skip posts to this row, naming the tab to return to, body:\n{unmatched}"
        );

        app.post_htmx(
            &format!("/admin/salary-imports/{import_id}/rows/{row_id}"),
            "gsis_player_id=00-7",
        )
        .await;
        let resolved = app
            .get(&format!(
                "/admin/salary-imports/{import_id}/triage?group=resolved"
            ))
            .await
            .text();
        assert!(
            resolved.contains(&format!(
                "hx-post=\"/admin/salary-imports/{import_id}/rows/{row_id}/undo?group=resolved\""
            )),
            "undo posts to this row's undo endpoint, naming the tab, body:\n{resolved}"
        );

        for (label, markup) in [("skip", &unmatched), ("undo", &resolved)] {
            let form = &markup[markup.find("hx-post=").expect("a decision form")..];
            let form = &form[..form.find('>').expect("the tag closes")];
            assert!(
                form.contains("hx-target=\"#triage\""),
                "{label} swaps the whole group, tag was:\n{form}"
            );
            assert!(
                form.contains("hx-swap=\"outerHTML\""),
                "{label} replaces the fragment, tag was:\n{form}"
            );
        }
    }

    #[test]
    fn review_and_fragment_render() {
        let results = ResultsFragment::new(
            1,
            1,
            vec![Candidate {
                gsis_id: "00-1".into(),
                label: "X (WR)".into(),
                sort_key: "x".into(),
            }],
            "on BUF, week 1".into(),
            false,
            false,
        )
        .render()
        .expect("results");
        assert!(results.contains("X (WR)"));

        // The row ships an empty container; htmx fills it from `/players`.
        let unmatched = ReviewRow {
            id: 1,
            fd_name: "Nobody Here".into(),
            fd_team: "BUF".into(),
            dfs_position: "WR".into(),
            salary: 5000,
            is_unmatched: true,
            state_label: String::new(),
            resolved_label: String::new(),
            suggestion: None,
        };
        let resolved = ReviewRow {
            id: 2,
            fd_name: "Nobody Here".into(),
            fd_team: "BUF".into(),
            dfs_position: "WR".into(),
            salary: 5000,
            is_unmatched: false,
            state_label: "resolved".into(),
            resolved_label: "Josh Allen (QB)".into(),
            suggestion: None,
        };

        let view = |rows| TriageView {
            import_id: 1,
            group_slug: RowGroup::Resolved.slug(),
            status: "pending".into(),
            auto_matched: 3,
            matched: 2,
            dst: 1,
            unmatched: 1,
            tabs: RowGroup::ALL
                .into_iter()
                .map(|g| GroupTab {
                    slug: g.slug(),
                    label: g.label(),
                    count: 0,
                    selected: g == RowGroup::Resolved,
                    url: format!("/admin/salary-imports/1{}", g.path_suffix()),
                })
                .collect(),
            rows,
            empty_message: RowGroup::Resolved.empty_message().into(),
        };

        // The block and the whole page render the same rows from one template,
        // so checking either proves both. `as_triage()` is the standalone render
        // used for the htmx swap; asserting the row header (`fd_name`) proves the
        // file-scoped `row_header` macro is in scope for the block.
        let fragment = ReviewTemplate {
            triage: view(vec![unmatched, resolved]),
            chrome: import_chrome(1),
        }
        .as_triage()
        .render()
        .expect("triage");
        assert!(fragment.contains("id=\"row-1-results\"></div>"));
        assert!(fragment.contains("Josh Allen (QB)"));
        assert!(fragment.contains("Nobody Here"));

        ReviewTemplate {
            triage: view(vec![]),
            chrome: import_chrome(1),
        }
        .render()
        .expect("review");
    }
}
