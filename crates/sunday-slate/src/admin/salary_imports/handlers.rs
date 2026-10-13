use askama::Template;
use axum::{
    Router,
    extract::{Multipart, Path, State},
    response::{Html, IntoResponse, Redirect},
    routing::{get, post},
};
use nfl_data::{Season, SeasonType, SeasonWeek, Week};
use serde::Deserialize;

use crate::admin::salary_imports::view::{DashboardTemplate, WeekTemplate};
use crate::admin::salary_imports::{self as import, default_season_week};
use crate::auth::AdminUser;
use crate::{AppError, AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(index).post(upload))
        .route("/{week}", get(week))
        .route("/{week}/players/{fd_player_id}", get(players))
        .route("/{week}/players/{fd_player_id}/search", get(player_search))
        .route("/{week}/players/{fd_player_id}/resolve", post(resolve))
}

pub async fn index(State(state): State<AppState>) -> Result<impl IntoResponse, AppError> {
    let season = Season(state.config.season);
    let games = state
        .nfl
        .games(season)
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;
    let default_week = default_season_week(&games, season, state.config.now());
    let counts = state
        .nfl
        .salary_counts_by_week()
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;
    let mut weeks = Vec::new();
    for (season_week, salary_count) in counts {
        let overview = import::salary_week_overview(&state.db, &state.nfl, season_week.clone())
            .await
            .map_err(|e| AppError::Internal(e.to_string()))?;
        let label = if season_week.season_type == SeasonType::Post {
            "POST"
        } else {
            "REG"
        }
        .to_owned();
        weeks.push((
            season_week.encode(),
            season_week.season.0,
            label,
            season_week.week.0,
            salary_count,
            overview.unmatched.len(),
        ));
    }
    let default_post = default_week.season_type == SeasonType::Post;
    Ok(Html(
        DashboardTemplate {
            weeks,
            default_season: default_week.season.0,
            default_week: default_week.week.0,
            default_post,
            chrome: crate::chrome::Chrome::focused("Salary weeks", "/admin"),
        }
        .render()?,
    ))
}

pub async fn upload(
    AdminUser(_): AdminUser,
    State(state): State<AppState>,
    mut mp: Multipart,
) -> Result<impl IntoResponse, AppError> {
    let mut season = None;
    let mut season_type = None;
    let mut week = None;
    let mut csv = None;
    while let Some(field) = mp
        .next_field()
        .await
        .map_err(|e| AppError::BadRequest(e.to_string()))?
    {
        match field.name().unwrap_or_default() {
            "season" => {
                season = Some(
                    field
                        .text()
                        .await
                        .map_err(|e| AppError::BadRequest(e.to_string()))?,
                )
            }
            "season_type" => {
                season_type = Some(
                    field
                        .text()
                        .await
                        .map_err(|e| AppError::BadRequest(e.to_string()))?,
                )
            }
            "week" => {
                week = Some(
                    field
                        .text()
                        .await
                        .map_err(|e| AppError::BadRequest(e.to_string()))?,
                )
            }
            "csv" => {
                csv = Some(
                    field
                        .bytes()
                        .await
                        .map_err(|e| AppError::BadRequest(e.to_string()))?
                        .to_vec(),
                )
            }
            _ => {}
        }
    }
    let season: u16 = season
        .and_then(|v| v.parse().ok())
        .filter(|v| (1900..=2200).contains(v))
        .ok_or_else(|| AppError::BadRequest("invalid or missing season".into()))?;
    let season_type = match season_type.as_deref() {
        Some("REG") => SeasonType::Reg,
        Some("POST") => SeasonType::Post,
        _ => {
            return Err(AppError::BadRequest(
                "invalid or missing season_type".into(),
            ));
        }
    };
    let week: u8 = week
        .and_then(|v| v.parse().ok())
        .filter(|v| {
            *v > 0
                && *v
                    <= if season_type == SeasonType::Reg {
                        18
                    } else {
                        22
                    }
        })
        .ok_or_else(|| AppError::BadRequest("invalid or missing week".into()))?;
    let csv = csv
        .filter(|bytes| !bytes.is_empty())
        .ok_or_else(|| AppError::BadRequest("missing csv file".into()))?;
    let season_week = SeasonWeek {
        season: Season(season),
        season_type,
        week: Week(week),
    };
    import::import_salary_csv(&state.db, &state.nfl, season_week.clone(), &csv)
        .await
        .map_err(|e| AppError::BadRequest(e.to_string()))?;
    Ok(Redirect::to(&format!(
        "/admin/salary-imports/{}",
        season_week.encode()
    )))
}

pub async fn week(
    State(state): State<AppState>,
    Path(encoded): Path<u32>,
) -> Result<impl IntoResponse, AppError> {
    let season_week = SeasonWeek::decode(encoded).ok_or(AppError::NotFound)?;
    let overview = import::salary_week_overview(&state.db, &state.nfl, season_week.clone())
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;
    Ok(Html(WeekTemplate::new(overview).render()?))
}

#[derive(Deserialize, Default)]
pub struct PlayerSearch {
    #[serde(default)]
    q: String,
    #[serde(default)]
    scope: Option<String>,
}

pub async fn players(
    State(state): State<AppState>,
    Path((encoded, fd_player_id)): Path<(u32, String)>,
    axum::extract::Query(query): axum::extract::Query<PlayerSearch>,
) -> Result<impl IntoResponse, AppError> {
    let season_week = SeasonWeek::decode(encoded).ok_or(AppError::NotFound)?;
    let row = unmatched_salary(&state, &season_week, &fd_player_id).await?;
    let results = import::candidates::search(
        &state,
        &season_week,
        &row.team,
        &query.q,
        query.scope.as_deref() == Some("all"),
    )
    .await?;
    let player = row.player.as_ref().ok_or(AppError::NotFound)?;
    let suggestion = import::candidates::suggestion_for(&state, &season_week, &row).await?;
    Ok(Html(
        crate::admin::salary_imports::view::ResolutionTemplate {
            week: encoded,
            fd_player_id,
            source_name: format!("{} {}", player.first_name, player.last_name),
            team: row.team.as_str().to_owned(),
            position: row.position.label().to_owned(),
            salary: row.salary,
            suggestion,
            results,
            query_encoded: urlencoding::encode(&query.q).into_owned(),
            query: query.q,
            chrome: crate::chrome::Chrome::focused(
                "Resolve salary player",
                format!("/admin/salary-imports/{encoded}"),
            ),
        }
        .render()?,
    ))
}

async fn unmatched_salary(
    state: &AppState,
    season_week: &SeasonWeek,
    fd_player_id: &str,
) -> Result<nfl_data::SalariedPlayer, AppError> {
    let overview = import::salary_week_overview(&state.db, &state.nfl, season_week.clone())
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;
    overview
        .unmatched
        .into_iter()
        .find(|salary| {
            salary
                .player
                .as_ref()
                .is_some_and(|player| player.id.to_string() == fd_player_id)
        })
        .ok_or(AppError::NotFound)
}

pub async fn player_search(
    State(state): State<AppState>,
    Path((encoded, fd_player_id)): Path<(u32, String)>,
    axum::extract::Query(query): axum::extract::Query<PlayerSearch>,
) -> Result<impl IntoResponse, AppError> {
    let season_week = SeasonWeek::decode(encoded).ok_or(AppError::NotFound)?;
    let row = unmatched_salary(&state, &season_week, &fd_player_id).await?;
    let results = import::candidates::search(
        &state,
        &season_week,
        &row.team,
        &query.q,
        query.scope.as_deref() == Some("all"),
    )
    .await?;
    Ok(Html(
        crate::admin::salary_imports::view::CandidateResultsTemplate {
            week: encoded,
            fd_player_id,
            results,
            query_encoded: urlencoding::encode(&query.q).into_owned(),
        }
        .render()?,
    ))
}

#[derive(Deserialize)]
pub struct ResolveForm {
    gsis_player_id: String,
}

pub async fn resolve(
    State(state): State<AppState>,
    Path((encoded, fd_player_id)): Path<(u32, String)>,
    axum::Form(form): axum::Form<ResolveForm>,
) -> Result<impl IntoResponse, AppError> {
    let season_week = SeasonWeek::decode(encoded).ok_or(AppError::NotFound)?;
    let identities = state
        .nfl
        .identify(&[form.gsis_player_id.as_str()])
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;
    if !identities.contains_key(&form.gsis_player_id) {
        return Err(AppError::BadRequest("unknown GSIS player ID".into()));
    }
    import::resolve_salary_player(
        &state.db,
        &state.nfl,
        season_week.clone(),
        &fd_player_id,
        &form.gsis_player_id,
    )
    .await
    .map_err(|e| AppError::BadRequest(e.to_string()))?;
    Ok(Redirect::to(&format!("/admin/salary-imports/{encoded}")))
}

#[cfg(test)]
mod tests {
    use crate::tests::factories::UserOptions;
    use crate::tests::{TestApp, factories};
    use axum_test::multipart::{MultipartForm, Part};
    use http::StatusCode;
    use nfl_data::{Game, Player, Season, SeasonType, Week};

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
    async fn admin_sees_dashboard() {
        let app = TestApp::new().await;
        app.login_admin().await;
        app.get("/admin/salary-imports").await.assert_status_ok();
    }

    #[tokio::test]
    async fn upload_uses_the_explicit_season_week() {
        let app = TestApp::new().await;
        app.login_admin().await;
        app.nfl
            .seed_for_test(
                &[Player {
                    gsis_id: "00-1".into(),
                    espn_id: None,
                    full_name: "Josh Allen".into(),
                    first_name: Some("Josh".into()),
                    last_name: Some("Allen".into()),
                    position: Some("QB".into()),
                    latest_team: Some("BUF".parse().unwrap()),
                    headshot_url: None,
                }],
                &[Game {
                    gsis_game_id: "2025_01_BUF_NYJ".into(),
                    season: Season(2025),
                    week: Week(1),
                    season_type: SeasonType::Reg,
                    kickoff: None,
                    home_team: "NYJ".parse().unwrap(),
                    away_team: "BUF".parse().unwrap(),
                    home_score: None,
                    away_score: None,
                }],
            )
            .await
            .unwrap();
        let csv = r#""Id","Position","First Name","Nickname","Last Name","FPPG","Played","Salary","Game","Team","Opponent","Injury Indicator","Injury Details"
"1-100","QB","Josh","","Allen","0","0","8000","BUF@NYJ","BUF","NYJ","",""
"#;
        let form = MultipartForm::new()
            .add_text("season", "2025")
            .add_text("season_type", "REG")
            .add_text("week", "1")
            .add_part(
                "csv",
                Part::bytes(csv.as_bytes().to_vec()).file_name("salary.csv"),
            );
        let response = app
            .server
            .post("/admin/salary-imports")
            .multipart(form)
            .await;
        response.assert_status(StatusCode::SEE_OTHER);
        assert_eq!(
            response.header("location").to_str().unwrap(),
            "/admin/salary-imports/2025001"
        );
    }

    #[tokio::test]
    async fn upload_rejects_invalid_season_type() {
        let app = TestApp::new().await;
        app.login_admin().await;
        let form = MultipartForm::new()
            .add_text("season", "2025")
            .add_text("season_type", "PLAYOFF")
            .add_text("week", "1")
            .add_part("csv", Part::bytes(b"csv".to_vec()).file_name("salary.csv"));
        app.server
            .post("/admin/salary-imports")
            .multipart(form)
            .await
            .assert_status(StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn upload_rejects_missing_season_type() {
        let app = TestApp::new().await;
        app.login_admin().await;
        let form = MultipartForm::new()
            .add_text("season", "2025")
            .add_text("week", "1")
            .add_part("csv", Part::bytes(b"csv".to_vec()).file_name("salary.csv"));
        app.server
            .post("/admin/salary-imports")
            .multipart(form)
            .await
            .assert_status(StatusCode::BAD_REQUEST);
    }
    const DASHBOARD_CSV: &str = r#""Id","Position","First Name","Nickname","Last Name","FPPG","Played","Salary","Game","Team","Opponent","Injury Indicator","Injury Details"
"1-100","QB","Josh","","Allen","0","0","8000","BUF@NYJ","BUF","NYJ","",""
"1-999","WR","Mystery","","Receiver","0","0","5000","BUF@NYJ","BUF","NYJ","",""
"1-200","D","Buffalo","","Bills","0","0","4000","BUF@NYJ","BUF","NYJ","",""
"#;

    async fn dashboard_app() -> TestApp {
        let app = TestApp::new().await;
        app.login_admin().await;
        app.nfl
            .seed_for_test(
                &[Player {
                    gsis_id: "00-1".into(),
                    espn_id: None,
                    full_name: "Josh Allen".into(),
                    first_name: Some("Josh".into()),
                    last_name: Some("Allen".into()),
                    position: Some("QB".into()),
                    latest_team: Some("BUF".parse().unwrap()),
                    headshot_url: None,
                }],
                &[Game {
                    gsis_game_id: "2025_01_BUF_NYJ".into(),
                    season: Season(2025),
                    week: Week(1),
                    season_type: SeasonType::Reg,
                    kickoff: None,
                    home_team: "NYJ".parse().unwrap(),
                    away_team: "BUF".parse().unwrap(),
                    home_score: None,
                    away_score: None,
                }],
            )
            .await
            .unwrap();
        app.nfl
            .seed_weekly_roster_for_test(&[nfl_data::WeeklyRosterEntry {
                season: Season(2025),
                week: Week(1),
                team: "BUF".parse().unwrap(),
                gsis_id: Some("00-roster-only".into()),
                espn_id: None,
                full_name: "Roster Receiver".into(),
                last_name: Some("Receiver".into()),
                position: Some("WR".into()),
            }])
            .await
            .unwrap();
        app
    }

    fn upload_form() -> MultipartForm {
        MultipartForm::new()
            .add_text("season", "2025")
            .add_text("season_type", "REG")
            .add_text("week", "1")
            .add_part(
                "csv",
                Part::bytes(DASHBOARD_CSV.as_bytes().to_vec()).file_name("salary.csv"),
            )
    }

    #[tokio::test]
    async fn dashboard_lists_source_counts_and_resolves_roster_only_players_directly() {
        let app = dashboard_app().await;
        app.server
            .post("/admin/salary-imports")
            .multipart(upload_form())
            .await
            .assert_status(StatusCode::SEE_OTHER);
        let salaries = crate::player_salaries::store::for_games(&app.pool, &["2025_01_BUF_NYJ"])
            .await
            .unwrap();
        assert_eq!(
            salaries.len(),
            2,
            "mapped QB and DST are immediately usable"
        );
        assert!(
            salaries
                .iter()
                .any(|s| s.dfs_position == nfl_data::DfsPosition::Dst)
        );
        let root = app.get("/admin/salary-imports").await;
        root.assert_status_ok();
        root.assert_text_contains("3 salaries");
        root.assert_text_contains("1 unresolved");
        root.assert_text_contains("/admin/salary-imports/2025001");
        let detail = app.get("/admin/salary-imports/2025001").await;
        detail.assert_status_ok();
        detail.assert_text_contains("Mystery Receiver");
        detail.assert_text_contains("/admin/salary-imports/2025001/players/999");
        assert!(!detail.text().contains("Josh Allen"));
        let candidates = app.get("/admin/salary-imports/2025001/players/999").await;
        candidates.assert_status_ok();
        candidates.assert_text_contains("Roster Receiver (WR)");
        candidates.assert_text_contains("on BUF, week 1");
        candidates.assert_text_contains("00-roster-only");
        app.server
            .post("/admin/salary-imports/2025001/players/999/resolve")
            .form(&[("gsis_player_id", "00-roster-only")])
            .await
            .assert_status(StatusCode::SEE_OTHER);
        let resolved = app.get("/admin/salary-imports/2025001").await;
        resolved.assert_text_contains("All player IDs are resolved");
        assert_eq!(
            crate::player_salaries::store::for_games(&app.pool, &["2025_01_BUF_NYJ"])
                .await
                .unwrap()
                .len(),
            3
        );
        assert_eq!(
            crate::nfl_players::store::by_fd_id(&app.pool, "999")
                .await
                .unwrap()
                .unwrap()
                .gsis_player_id,
            "00-roster-only"
        );
    }

    #[tokio::test]
    async fn postseason_upload_redirects_to_the_complete_week_and_projects_salaries() {
        let app = dashboard_app().await;
        app.nfl
            .seed_for_test(
                &[],
                &[Game {
                    gsis_game_id: "2025_21_BUF_NYJ".into(),
                    season: Season(2025),
                    week: Week(21),
                    season_type: SeasonType::Post,
                    kickoff: None,
                    home_team: "NYJ".parse().unwrap(),
                    away_team: "BUF".parse().unwrap(),
                    home_score: None,
                    away_score: None,
                }],
            )
            .await
            .unwrap();
        let form = MultipartForm::new()
            .add_text("season", "2025")
            .add_text("season_type", "POST")
            .add_text("week", "21")
            .add_part(
                "csv",
                Part::bytes(DASHBOARD_CSV.as_bytes().to_vec()).file_name("salary.csv"),
            );
        let response = app
            .server
            .post("/admin/salary-imports")
            .multipart(form)
            .await;
        response.assert_status(StatusCode::SEE_OTHER);
        assert_eq!(
            response.header("location").to_str().unwrap(),
            "/admin/salary-imports/2025121"
        );
        assert_eq!(
            crate::player_salaries::store::for_games(&app.pool, &["2025_21_BUF_NYJ"])
                .await
                .unwrap()
                .len(),
            2
        );
        assert!(
            crate::player_salaries::store::for_games(&app.pool, &["2025_01_BUF_NYJ"])
                .await
                .unwrap()
                .is_empty()
        );
        app.get("/admin/salary-imports/2025121")
            .await
            .assert_text_contains("POST");
    }

    #[tokio::test]
    async fn dashboard_groups_sources_by_season_type_and_week() {
        let app = dashboard_app().await;
        for season_week in [
            nfl_data::SeasonWeek {
                season: Season(2025),
                season_type: SeasonType::Reg,
                week: Week(1),
            },
            nfl_data::SeasonWeek {
                season: Season(2025),
                season_type: SeasonType::Post,
                week: Week(21),
            },
            nfl_data::SeasonWeek {
                season: Season(2026),
                season_type: SeasonType::Reg,
                week: Week(1),
            },
        ] {
            app.nfl
                .import_fanduel_csv(season_week, DASHBOARD_CSV.as_bytes())
                .await
                .unwrap();
        }
        let root = app.get("/admin/salary-imports").await;
        root.assert_status_ok();
        for encoded in [2025001, 2025121, 2026001] {
            root.assert_text_contains(format!("/admin/salary-imports/{encoded}"));
        }
        assert_eq!(root.text().matches("3 salaries").count(), 3);
        assert_eq!(root.text().matches("2 unresolved").count(), 3);
    }

    #[tokio::test]
    async fn candidate_search_keeps_week_scope_until_explicitly_widened() {
        let app = dashboard_app().await;
        app.server
            .post("/admin/salary-imports")
            .multipart(upload_form())
            .await
            .assert_status(StatusCode::SEE_OTHER);
        app.get("/admin/salary-imports/2025001/players/999?q=Roster")
            .await
            .assert_text_contains("Roster Receiver");
        let narrow = app
            .get("/admin/salary-imports/2025001/players/999?q=Josh")
            .await;
        narrow.assert_text_contains("No matching players found");
        let wide = app
            .get("/admin/salary-imports/2025001/players/999?q=Josh&scope=all")
            .await;
        wide.assert_text_contains("Josh Allen (QB)");
        wide.assert_text_contains("across all teams and weeks");
        app.get("/admin/salary-imports/2025001/players/999?scope=all&q=J")
            .await
            .assert_text_contains("Type at least two characters");
    }

    #[tokio::test]
    async fn resolution_page_shows_source_context_and_confirmable_weekly_suggestion() {
        let app = dashboard_app().await;
        app.server
            .post("/admin/salary-imports")
            .multipart(upload_form())
            .await
            .assert_status(StatusCode::SEE_OTHER);

        let response = app.get("/admin/salary-imports/2025001/players/999").await;
        response.assert_text_contains("Mystery Receiver");
        response.assert_text_contains("BUF · WR · $5000");
        response.assert_text_contains("Looks like");
        response.assert_text_contains("Roster Receiver (WR)");
        response.assert_text_contains("Confirm suggestion");
        response.assert_text_contains("name=\"gsis_player_id\" value=\"00-roster-only\"");
        assert!(
            crate::nfl_players::store::by_fd_id(&app.pool, "999")
                .await
                .unwrap()
                .is_none(),
            "showing a suggestion must not create the crosswalk"
        );
    }

    #[tokio::test]
    async fn changing_candidate_scope_preserves_the_search_query() {
        let app = dashboard_app().await;
        app.server
            .post("/admin/salary-imports")
            .multipart(upload_form())
            .await
            .assert_status(StatusCode::SEE_OTHER);

        let narrow = app
            .get("/admin/salary-imports/2025001/players/999?q=Josh")
            .await;
        narrow.assert_text_contains("q=Josh&amp;scope=all");
        let widened = app
            .get("/admin/salary-imports/2025001/players/999?q=Josh&scope=all")
            .await;
        widened.assert_text_contains("Josh Allen (QB)");
        widened.assert_text_contains("q=Josh");
        widened.assert_text_contains("Back to this team's week roster");
    }

    #[tokio::test]
    async fn resolution_typeahead_returns_a_fragment_without_displaying_ids() {
        let app = dashboard_app().await;
        app.server
            .post("/admin/salary-imports")
            .multipart(upload_form())
            .await
            .assert_status(StatusCode::SEE_OTHER);

        let page = app.get("/admin/salary-imports/2025001/players/999").await;
        page.assert_text_contains("hx-trigger=\"focus once, input changed delay:300ms\"");
        page.assert_text_contains("hx-target=\"#candidate-results\"");
        assert!(!page.text().contains("ID 999"));
        assert!(
            !page
                .text()
                .contains("Roster Receiver (WR) · 00-roster-only")
        );

        let results = app
            .get("/admin/salary-imports/2025001/players/999/search?q=Roster")
            .await;
        results.assert_text_contains("Roster Receiver (WR)");
        results.assert_text_contains("on BUF, week 1");
        assert!(!results.text().contains("<html"));

        app.get("/admin/salary-imports/2025001/players/999/search?q=Josh&scope=all")
            .await
            .assert_text_contains("Josh Allen (QB)");
    }

    #[tokio::test]
    async fn upload_rejects_every_missing_or_invalid_field() {
        let app = TestApp::new().await;
        app.login_admin().await;
        for (field, value) in [
            ("season", None),
            ("season", Some("x")),
            ("season", Some("1899")),
            ("season_type", None),
            ("season_type", Some("PRE")),
            ("week", None),
            ("week", Some("x")),
            ("week", Some("0")),
            ("week", Some("19")),
            ("csv", None),
            ("csv", Some("")),
        ] {
            let mut form = MultipartForm::new();
            for (name, default) in [
                ("season", "2025"),
                ("season_type", "REG"),
                ("week", "1"),
                ("csv", "invalid csv"),
            ] {
                if let Some(v) = if name == field { value } else { Some(default) } {
                    form = if name == "csv" {
                        form.add_part(
                            name,
                            Part::bytes(v.as_bytes().to_vec()).file_name("salary.csv"),
                        )
                    } else {
                        form.add_text(name, v)
                    };
                }
            }
            app.server
                .post("/admin/salary-imports")
                .multipart(form)
                .await
                .assert_status(StatusCode::BAD_REQUEST);
        }
        assert!(app.nfl.salary_counts_by_week().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn every_dashboard_route_requires_admin_authorization() {
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
        for path in [
            "/admin/salary-imports",
            "/admin/salary-imports/2025001",
            "/admin/salary-imports/2025001/players/999",
        ] {
            app.get(path).await.assert_status(StatusCode::FORBIDDEN);
        }
        for path in [
            "/admin/salary-imports",
            "/admin/salary-imports/2025001/players/999/resolve",
        ] {
            app.server
                .post(path)
                .await
                .assert_status(StatusCode::FORBIDDEN);
        }
    }

    #[tokio::test]
    async fn anonymous_dashboard_requests_redirect_to_login() {
        let app = TestApp::new().await;
        app.get("/admin/salary-imports/2025001")
            .await
            .assert_status(StatusCode::TEMPORARY_REDIRECT);
        app.server
            .post("/admin/salary-imports/2025001/players/999/resolve")
            .await
            .assert_status(StatusCode::TEMPORARY_REDIRECT);
    }

    #[tokio::test]
    async fn old_staged_routes_are_absent_and_unknown_players_are_rejected() {
        let app = dashboard_app().await;
        app.server
            .post("/admin/salary-imports")
            .multipart(upload_form())
            .await
            .assert_status(StatusCode::SEE_OTHER);
        for action in [
            "commit",
            "discard",
            "triage",
            "skipped",
            "resolved",
            "rows/1/undo",
        ] {
            app.server
                .post(&format!("/admin/salary-imports/1/{action}"))
                .await
                .assert_status(StatusCode::NOT_FOUND);
        }
        app.get("/admin/salary-imports/2025201")
            .await
            .assert_status(StatusCode::NOT_FOUND);
        app.get("/admin/salary-imports/2025001/players/404")
            .await
            .assert_status(StatusCode::NOT_FOUND);
        app.server
            .post("/admin/salary-imports/2025001/players/999/resolve")
            .form(&[("gsis_player_id", "not-a-player")])
            .await
            .assert_status(StatusCode::BAD_REQUEST);
        app.server
            .post("/admin/salary-imports/2025001/players/404/resolve")
            .form(&[("gsis_player_id", "00-1")])
            .await
            .assert_status(StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn dashboard_form_uses_config_clock_at_the_monday_boundary() {
        use time::macros::datetime;
        let app = TestApp::from_pool_at(
            crate::tests::utils::in_memory_pool().await,
            datetime!(2025-10-06 18:00 UTC),
        )
        .await;
        app.login_admin().await;
        let game = |id: &str, week, kickoff| Game {
            gsis_game_id: id.into(),
            season: Season(2025),
            week: Week(week),
            season_type: SeasonType::Reg,
            kickoff: Some(kickoff),
            home_team: "NYJ".parse().unwrap(),
            away_team: "BUF".parse().unwrap(),
            home_score: None,
            away_score: None,
        };
        app.nfl
            .seed_for_test(
                &[],
                &[
                    game("monday", 5, datetime!(2025-10-06 23:15 UTC)),
                    game("upcoming", 6, datetime!(2025-10-12 17:00 UTC)),
                ],
            )
            .await
            .unwrap();
        app.get("/admin/salary-imports")
            .await
            .assert_text_contains("value=\"6\"");
    }
}
