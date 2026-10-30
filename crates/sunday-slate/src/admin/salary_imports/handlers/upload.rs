use crate::admin::salary_imports as import;
use crate::auth::AdminUser;
use crate::{AppError, AppState};
use axum::extract::{Multipart, State};
use axum::response::{IntoResponse, Redirect};
use nfl_data::{Season, SeasonType, SeasonWeek, Week};

struct SalaryUpload {
    season_week: SeasonWeek,
    csv: Vec<u8>,
}

pub async fn upload(
    AdminUser(_): AdminUser,
    State(state): State<AppState>,
    mp: Multipart,
) -> Result<impl IntoResponse, AppError> {
    let SalaryUpload { season_week, csv } = read_upload(mp).await?;
    import::import_salary_csv(&state.db, &state.nfl, season_week, &csv)
        .await
        .map_err(|e| AppError::BadRequest(e.to_string()))?;
    Ok(Redirect::to(&format!(
        "/admin/salary-imports/{}",
        season_week.encode()
    )))
}

async fn read_upload(mut mp: Multipart) -> Result<SalaryUpload, AppError> {
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
    Ok(SalaryUpload { season_week, csv })
}

#[cfg(test)]
mod tests {
    use super::super::tests::{DASHBOARD_CSV, dashboard_app};
    use crate::tests::TestApp;
    use axum_test::multipart::{MultipartForm, Part};
    use http::StatusCode;
    use nfl_data::{Game, Player, Season, SeasonType, Week};

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
                    game_id: "2025_01_BUF_NYJ".into(),
                    season_week: nfl_data::SeasonWeek {
                        season: Season(2025),
                        season_type: SeasonType::Reg,
                        week: Week(1),
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

    #[tokio::test]
    async fn postseason_upload_redirects_to_the_complete_week_and_projects_salaries() {
        let app = dashboard_app().await;
        app.nfl
            .seed_for_test(
                &[],
                &[Game {
                    game_id: "2025_21_BUF_NYJ".into(),
                    season_week: nfl_data::SeasonWeek {
                        season: Season(2025),
                        season_type: SeasonType::Post,
                        week: Week(21),
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
        assert!(
            app.nfl
                .fanduel_weekly_summaries(&[])
                .await
                .unwrap()
                .is_empty()
        );
    }
}
