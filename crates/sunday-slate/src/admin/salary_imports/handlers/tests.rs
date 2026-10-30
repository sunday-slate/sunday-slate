use crate::tests::factories::UserOptions;
use crate::tests::{TestApp, factories};
use axum_test::multipart::{MultipartForm, Part};
use http::StatusCode;
use nfl_data::{Game, Player, Season, SeasonType, Week};

pub(super) const DASHBOARD_CSV: &str = r#""Id","Position","First Name","Nickname","Last Name","FPPG","Played","Salary","Game","Team","Opponent","Injury Indicator","Injury Details"
"1-100","QB","Josh","","Allen","0","0","8000","BUF@NYJ","BUF","NYJ","",""
"1-999","WR","Mystery","","Receiver","0","0","5000","BUF@NYJ","BUF","NYJ","",""
"1-200","D","Buffalo","","Bills","0","0","4000","BUF@NYJ","BUF","NYJ","",""
"#;

pub(super) async fn dashboard_app() -> TestApp {
    let app = TestApp::new().await;
    app.login_admin().await;
    app.nfl
        .seed_for_test(
            &[
                Player {
                    gsis_id: "00-1".into(),
                    espn_id: None,
                    full_name: "Josh Allen".into(),
                    first_name: Some("Josh".into()),
                    last_name: Some("Allen".into()),
                    position: Some("QB".into()),
                    latest_team: Some("BUF".parse().unwrap()),
                    headshot_url: None,
                },
                Player {
                    gsis_id: "00-avery".into(),
                    espn_id: None,
                    full_name: "Avery Smith".into(),
                    first_name: Some("Avery".into()),
                    last_name: Some("Smith".into()),
                    position: Some("WR".into()),
                    latest_team: Some("ATL".parse().unwrap()),
                    headshot_url: None,
                },
            ],
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

pub(super) fn upload_form() -> MultipartForm {
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
async fn old_staged_routes_are_absent() {
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
}
