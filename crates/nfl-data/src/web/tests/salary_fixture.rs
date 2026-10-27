use std::sync::Arc;

use axum::Router;
use axum_test::TestServer;
use axum_test::multipart::{MultipartForm, Part};
use time::macros::datetime;

use crate::web::AdminConfig;
use crate::{NflData, Season};

pub(crate) struct AdminFixture {
    pub(crate) server: TestServer,
    pub(crate) nfl: Arc<NflData>,
}

pub(crate) async fn admin_state() -> AdminFixture {
    let nfl = Arc::new(NflData::in_memory().await.unwrap());
    nfl.seed_for_test(
        &[
            crate::Player {
                gsis_id: "00-1".into(),
                espn_id: None,
                full_name: "Josh Allen".into(),
                first_name: Some("Josh".into()),
                last_name: Some("Allen".into()),
                position: Some("QB".into()),
                latest_team: Some("BUF".parse().unwrap()),
                headshot_url: None,
            },
            crate::Player {
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
        &[crate::Game {
            gsis_game_id: "2025_01_BUF_NYJ".into(),
            season: Season(2025),
            week: crate::Week(1),
            season_type: crate::SeasonType::Reg,
            kickoff: None,
            home_team: "NYJ".parse().unwrap(),
            away_team: "BUF".parse().unwrap(),
            home_score: None,
            away_score: None,
        }],
    )
    .await
    .unwrap();
    nfl.seed_weekly_roster_for_test(&[crate::WeeklyRosterEntry {
        season: Season(2025),
        week: crate::Week(1),
        team: "BUF".parse().unwrap(),
        gsis_id: Some("00-roster-only".into()),
        espn_id: None,
        full_name: "Roster Receiver".into(),
        last_name: Some("Receiver".into()),
        position: Some("WR".into()),
    }])
    .await
    .unwrap();
    let config = AdminConfig {
        season: Season(2025),
        now_override: Some(datetime!(2025-10-06 18:00 UTC)),
    };
    let server = TestServer::new(Router::new().nest(
        "/nfl-data-admin",
        crate::admin_router::<()>(std::sync::Arc::clone(&nfl), config),
    ));
    AdminFixture { server, nfl }
}

pub(crate) const DASHBOARD_CSV: &str = r#""Id","Position","First Name","Nickname","Last Name","FPPG","Played","Salary","Game","Team","Opponent","Injury Indicator","Injury Details"
"1-100","QB","Josh","","Allen","0","0","8000","BUF@NYJ","BUF","NYJ","",""
"1-999","WR","Mystery","","Receiver","0","0","5000","BUF@NYJ","BUF","NYJ","",""
"1-888","WR","Second","","Receiver","0","0","4500","BUF@NYJ","BUF","NYJ","",""
"1-200","D","Buffalo","","Bills","0","0","4000","BUF@NYJ","BUF","NYJ","",""
"#;

pub(crate) const POST_CSV: &str = DASHBOARD_CSV;

pub(crate) fn upload_form() -> MultipartForm {
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
async fn every_salary_route_requires_authorization_in_the_host() {
    let state = admin_state().await;
    let page = state.server.get("/nfl-data-admin/salary-imports").await;
    page.assert_status_ok();
    let detail = state
        .server
        .get("/nfl-data-admin/salary-imports/2025001")
        .await;
    detail.assert_status_ok();
}

#[tokio::test]
async fn dashboard_lists_source_counts_and_resolves_roster_only_players_directly() {
    let state = admin_state().await;
    state
        .server
        .post("/nfl-data-admin/salary-imports")
        .multipart(upload_form())
        .await
        .assert_status(axum::http::StatusCode::SEE_OTHER);
    let overview = state
        .nfl
        .salary_week_overview(crate::SeasonWeek {
            season: Season(2025),
            season_type: crate::SeasonType::Reg,
            week: crate::Week(1),
        })
        .await
        .unwrap();
    assert_eq!(overview.salary_count, 4);
    assert_eq!(overview.unmatched.len(), 2);
    assert!(
        overview
            .unmatched
            .iter()
            .any(|row| row.player.as_ref().is_some_and(|p| p.id == 999))
    );

    let root = state.server.get("/nfl-data-admin/salary-imports").await;
    root.assert_status_ok();
    root.assert_text_contains("4 salaries");
    root.assert_text_contains("2 unresolved");
    root.assert_text_contains("/nfl-data-admin/salary-imports/2025001");
    let detail = state
        .server
        .get("/nfl-data-admin/salary-imports/2025001")
        .await;
    detail.assert_status_ok();
    detail.assert_text_contains("Mystery Receiver");
    detail.assert_text_contains("/nfl-data-admin/salary-imports/2025001/players/999");
    assert!(!detail.text().contains("Josh Allen"));
    let resolution = state
        .server
        .get("/nfl-data-admin/salary-imports/2025001/players/999")
        .await;
    resolution.assert_status_ok();
    resolution.assert_text_contains("Confirm suggestion");
    let candidates = state
        .server
        .get("/nfl-data-admin/salary-imports/2025001/players/999/search?q=Roster")
        .await;
    candidates.assert_status_ok();
    candidates.assert_text_contains("Roster Receiver (WR)");
    candidates.assert_text_contains("on BUF");
    assert!(!candidates.text().contains("week 1"));
    assert!(!candidates.text().contains(">00-roster-only<"));
    state
        .server
        .post("/nfl-data-admin/salary-imports/2025001/players/999/resolve")
        .form(&[("gsis_player_id", "00-roster-only")])
        .await
        .assert_status(axum::http::StatusCode::SEE_OTHER);
    let resolved = state
        .server
        .get("/nfl-data-admin/salary-imports/2025001")
        .await;
    resolved.assert_text_contains("Second Receiver");
    assert!(!resolved.text().contains("All player IDs are resolved"));
    let overview = state
        .nfl
        .salary_week_overview(crate::SeasonWeek {
            season: Season(2025),
            season_type: crate::SeasonType::Reg,
            week: crate::Week(1),
        })
        .await
        .unwrap();
    assert_eq!(
        overview.unmatched.len(),
        1,
        "the second mystery row stays unresolved"
    );
    assert_eq!(
        state
            .nfl
            .player_link_by_fd_id(999)
            .await
            .unwrap()
            .unwrap()
            .gsis_player_id,
        "00-roster-only"
    );
}

#[tokio::test]
async fn dashboard_groups_sources_by_season_type_and_week() {
    let state = admin_state().await;
    for season_week in [
        crate::SeasonWeek {
            season: Season(2025),
            season_type: crate::SeasonType::Reg,
            week: crate::Week(1),
        },
        crate::SeasonWeek {
            season: Season(2025),
            season_type: crate::SeasonType::Post,
            week: crate::Week(21),
        },
        crate::SeasonWeek {
            season: Season(2026),
            season_type: crate::SeasonType::Reg,
            week: crate::Week(1),
        },
    ] {
        let salaries = state
            .nfl
            .parse_fanduel_csv(season_week, DASHBOARD_CSV.as_bytes())
            .unwrap();
        state.nfl.save_fanduel_salaries(&salaries).await.unwrap();
    }
    let root = state.server.get("/nfl-data-admin/salary-imports").await;
    root.assert_status_ok();
    for encoded in [2025001, 2025121, 2026001] {
        root.assert_text_contains(format!("/nfl-data-admin/salary-imports/{encoded}"));
    }
    assert_eq!(root.text().matches("4 salaries").count(), 3);
    assert_eq!(root.text().matches("3 unresolved").count(), 3);
}

#[tokio::test]
async fn dashboard_form_uses_config_clock_at_the_monday_boundary() {
    let state = admin_state().await;
    state
        .nfl
        .seed_for_test(
            &[],
            &[
                crate::Game {
                    gsis_game_id: "monday".into(),
                    season: Season(2025),
                    week: crate::Week(5),
                    season_type: crate::SeasonType::Reg,
                    kickoff: Some(datetime!(2025-10-06 23:15 UTC)),
                    home_team: "NYJ".parse().unwrap(),
                    away_team: "BUF".parse().unwrap(),
                    home_score: None,
                    away_score: None,
                },
                crate::Game {
                    gsis_game_id: "upcoming".into(),
                    season: Season(2025),
                    week: crate::Week(6),
                    season_type: crate::SeasonType::Reg,
                    kickoff: Some(datetime!(2025-10-12 17:00 UTC)),
                    home_team: "NYJ".parse().unwrap(),
                    away_team: "BUF".parse().unwrap(),
                    home_score: None,
                    away_score: None,
                },
            ],
        )
        .await
        .unwrap();
    let root = state.server.get("/nfl-data-admin/salary-imports").await;
    root.assert_text_contains("value=\"6\"");
}
