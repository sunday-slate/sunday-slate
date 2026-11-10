use crate::web::salary_imports::SalaryAdminError;
use crate::web::{AdminState, salary_imports};
use crate::{NflDataError, SalaryWeekOverview, salaries};
use axum::extract::{Multipart, State};
use axum::response::{IntoResponse, Redirect};
use nfl_model::SeasonWeek;

struct SalaryUpload {
    season_week: crate::SeasonWeek,
    csv: Vec<u8>,
}

pub async fn upload(
    State(state): State<AdminState>,
    mp: Multipart,
) -> Result<impl IntoResponse, SalaryAdminError> {
    let SalaryUpload { season_week, csv } = read_upload(mp).await?;
    let overview = import_salary_csv(&state, season_week, &csv)
        .await
        .map_err(|e| SalaryAdminError::BadRequest(e.to_string()))?;
    let season_type = if overview.season_week.postseason() {
        crate::SeasonType::Post
    } else {
        crate::SeasonType::Reg
    };
    tracing::info!(
        season = overview.season_week.season.0,
        week = overview.season_week.week.0,
        season_type = ?season_type,
        salary_count = overview.salary_count,
        "salary import processed"
    );
    Ok(Redirect::to(&format!(
        "/nfl-data-admin/salary-imports/{}",
        season_week.encode()
    )))
}

/// Parse the import form into a season week and the FanDuel CSV body.
///
/// The form posts `season`, `week`, and `csv`; `week` is the absolute week number, so 1–18 are
/// regular season and 19–22 are postseason. Unknown fields are ignored, which keeps a stale
/// page that still posts the retired `season_type` field working. Every other input is either
/// accepted as a valid [`SeasonWeek`] or rejected with a 400.
async fn read_upload(mut mp: Multipart) -> Result<SalaryUpload, SalaryAdminError> {
    let mut season = None;
    let mut week = None;
    let mut csv = None;
    while let Some(field) = mp
        .next_field()
        .await
        .map_err(|e| SalaryAdminError::BadRequest(e.to_string()))?
    {
        match field.name().unwrap_or_default() {
            "season" => {
                season = Some(
                    field
                        .text()
                        .await
                        .map_err(|e| SalaryAdminError::BadRequest(e.to_string()))?,
                )
            }
            "week" => {
                week = Some(
                    field
                        .text()
                        .await
                        .map_err(|e| SalaryAdminError::BadRequest(e.to_string()))?,
                )
            }
            "csv" => {
                let filename = field.file_name().map(str::to_owned);
                let content_type = field.content_type().map(str::to_owned);
                let bytes = field
                    .bytes()
                    .await
                    .map_err(|e| SalaryAdminError::BadRequest(e.to_string()))?
                    .to_vec();
                tracing::debug!(
                    target: "sunday_slate::http_input",
                    filename = ?filename,
                    content_type = ?content_type,
                    bytes = bytes.len(),
                    "salary csv upload"
                );
                csv = Some(bytes);
            }
            _ => {}
        }
    }
    let season: u16 = season
        .and_then(|v| v.parse().ok())
        .filter(|v| (1900..=2200).contains(v))
        .ok_or_else(|| SalaryAdminError::BadRequest("invalid or missing season".into()))?;
    let season_week = week
        .and_then(|v| v.parse::<u8>().ok())
        .and_then(|week| SeasonWeek::new(crate::Season(season), crate::Week(week)))
        .ok_or_else(|| SalaryAdminError::BadRequest("invalid or missing week".into()))?;
    let csv = csv
        .filter(|bytes| !bytes.is_empty())
        .ok_or_else(|| SalaryAdminError::BadRequest("missing csv file".into()))?;
    Ok(SalaryUpload { season_week, csv })
}

/// Validate the slate, persist the CSV, apply persisted links, and republish
/// the snapshot once.
///
/// Persistence happens before the rebuild, so a later failure still republishes
/// what was committed; the original failure is what the caller sees.
async fn import_salary_csv(
    state: &AdminState,
    season_week: SeasonWeek,
    csv_data: &[u8],
) -> Result<SalaryWeekOverview, NflDataError> {
    let source = salaries::parse_fanduel_csv(&state.nfl.fanduel, season_week, csv_data)?;
    let games = crate::provider::games(&state.nfl.provider, season_week.season).await?;
    salaries::resolve_slate_games(&games, &season_week, &salaries::salary_matchups(&source))
        .map_err(|_| NflDataError::Schedule)?;
    salaries::save_fanduel_salaries(&state.nfl.fanduel, &source).await?;
    let linked = salaries::reconcile_salary_week(
        &state.nfl.fanduel,
        &state.nfl.provider,
        &state.nfl.store,
        season_week,
    )
    .await;
    let rebuilt = state.nfl.reconcile_slates_and_rebuild().await;
    linked?;
    rebuilt?;
    salary_imports::salary_week_overview(state, season_week).await
}

#[cfg(test)]
mod tests {
    use crate::web::tests::salary_fixture::{
        admin_slate_id, admin_state, admin_state_in_season, upload_form,
    };
    use crate::{DfsPosition, Season};
    use axum::http::StatusCode;
    use axum_test::multipart::{MultipartForm, Part};
    use nfl_model::{DfsPositionSlotType, NflPlayerId};

    #[tokio::test]
    async fn upload_uses_the_explicit_season_week() {
        let state = admin_state().await;
        let csv = r#""Id","Position","First Name","Nickname","Last Name","FPPG","Played","Salary","Game","Team","Opponent","Injury Indicator","Injury Details"
"1-100","QB","Josh","","Allen","0","0","8000","BUF@NYJ","BUF","NYJ","",""
"#;
        let form = MultipartForm::new()
            .add_text("season", "2025")
            .add_text("week", "1")
            .add_part(
                "csv",
                Part::bytes(csv.as_bytes().to_vec()).file_name("salary.csv"),
            );
        let response = state
            .server
            .post("/nfl-data-admin/salary-imports")
            .multipart(form)
            .await;
        response.assert_status(StatusCode::SEE_OTHER);
        assert_eq!(
            response.header("location").to_str().unwrap(),
            "/nfl-data-admin/salary-imports/2025001"
        );
    }

    #[tokio::test]
    async fn upload_ignores_a_retired_season_type_field() {
        let state = admin_state().await;
        let csv = r#""Id","Position","First Name","Nickname","Last Name","FPPG","Played","Salary","Game","Team","Opponent","Injury Indicator","Injury Details"
"1-100","QB","Josh","","Allen","0","0","8000","BUF@NYJ","BUF","NYJ","",""
"#;
        let form = MultipartForm::new()
            .add_text("season", "2025")
            .add_text("season_type", "POST")
            .add_text("week", "1")
            .add_part(
                "csv",
                Part::bytes(csv.as_bytes().to_vec()).file_name("salary.csv"),
            );
        let response = state
            .server
            .post("/nfl-data-admin/salary-imports")
            .multipart(form)
            .await;
        response.assert_status(StatusCode::SEE_OTHER);
        assert_eq!(
            response.header("location").to_str().unwrap(),
            "/nfl-data-admin/salary-imports/2025001",
            "the absolute week decides the season week, not the retired field"
        );
        let page = state
            .server
            .get("/nfl-data-admin/salary-imports/2025001")
            .await;
        page.assert_text_contains("REG");
    }

    #[tokio::test]
    async fn postseason_upload_redirects_to_the_complete_week() {
        let state = admin_state().await;
        state
            .nfl
            .seed_for_test(
                &[],
                &[crate::Game {
                    game_id: "2025_21_BUF_NYJ".into(),
                    season_week: crate::SeasonWeek {
                        season: crate::Season(2025),
                        week: crate::Week(21),
                    },
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
            .add_text("week", "21")
            .add_part(
                "csv",
                Part::bytes(
                    crate::web::tests::salary_fixture::POST_CSV
                        .as_bytes()
                        .to_vec(),
                )
                .file_name("salary.csv"),
            );
        let response = state
            .server
            .post("/nfl-data-admin/salary-imports")
            .multipart(form)
            .await;
        response.assert_status(StatusCode::SEE_OTHER);
        assert_eq!(
            response.header("location").to_str().unwrap(),
            "/nfl-data-admin/salary-imports/2025121"
        );
        let page = state
            .server
            .get("/nfl-data-admin/salary-imports/2025121")
            .await;
        page.assert_text_contains("POST");
    }

    #[tokio::test]
    async fn upload_rejects_every_missing_or_invalid_field() {
        let state = admin_state().await;
        for (field, value) in [
            ("season", None),
            ("season", Some("x")),
            ("season", Some("1899")),
            ("week", None),
            ("week", Some("x")),
            ("week", Some("0")),
            ("week", Some("23")),
            ("week", Some("255")),
            ("csv", None),
            ("csv", Some("")),
        ] {
            let mut form = MultipartForm::new();
            for (name, default) in [("season", "2025"), ("week", "1"), ("csv", "invalid csv")] {
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
            state
                .server
                .post("/nfl-data-admin/salary-imports")
                .multipart(form)
                .await
                .assert_status(StatusCode::BAD_REQUEST);
        }
        assert!(
            crate::salaries::fanduel_weekly_summaries(&state.nfl.fanduel, &[])
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn uploaded_salaries_publish_matched_players_before_the_response() {
        let state = admin_state_in_season(Season(2025)).await;

        // Josh Allen auto-matches; the two receivers stay unmatched. The
        // handler rebuilds before responding, so no polling is involved.
        state
            .server
            .post("/nfl-data-admin/salary-imports")
            .multipart(upload_form())
            .await
            .assert_status(StatusCode::SEE_OTHER);

        let snapshot = state.nfl.snapshot();
        assert!(
            snapshot
                .player(&admin_slate_id(), &NflPlayerId("00-1".into()))
                .is_some_and(|player| player.name == "Josh Allen"),
            "the completed upload is visible immediately"
        );

        // The unmatched receivers published nothing.
        assert!(
            snapshot
                .salaried_players_for_position(
                    &admin_slate_id(),
                    &DfsPositionSlotType::Position(DfsPosition::Wr),
                )
                .is_empty()
        );
        assert!(
            snapshot
                .player(&admin_slate_id(), &NflPlayerId("00-roster-only".into()))
                .is_none()
        );
    }
}
