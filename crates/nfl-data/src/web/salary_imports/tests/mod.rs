use std::sync::Arc;

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use tower::ServiceExt;

use crate::{
    Game, NflData, Player, Season, SeasonType, TeamAbbr, Week,
    salary_imports::{model::RowState, service::SalaryImportService, store},
    web::router,
};
use time::macros::datetime;

const CSV: &str = "\"Id\",\"Position\",\"First Name\",\"Nickname\",\"Last Name\",\"FPPG\",\"Played\",\"Salary\",\"Game\",\"Team\",\"Opponent\",\"Injury Indicator\",\"Injury Details\"\n\"1-100\",\"QB\",\"Josh\",\"\",\"Allen\",\"0\",\"0\",\"8000\",\"BUF@NYJ\",\"BUF\",\"\",\"\",\"\"\n";
const UNMATCHED_CSV: &str = "\"Id\",\"Position\",\"First Name\",\"Nickname\",\"Last Name\",\"FPPG\",\"Played\",\"Salary\",\"Game\",\"Team\",\"Opponent\",\"Injury Indicator\",\"Injury Details\"\n\"1-101\",\"QB\",\"Unknown\",\"\",\"Player\",\"0\",\"0\",\"8000\",\"BUF@NYJ\",\"BUF\",\"\",\"\",\"\"\n";

async fn setup() -> (Router, Arc<NflData>, i64) {
    setup_with_csv(CSV).await
}

async fn setup_with_csv(csv: &str) -> (Router, Arc<NflData>, i64) {
    let nfl = Arc::new(NflData::in_memory().await.unwrap());
    nfl.seed_for_test(
        &[Player {
            gsis_id: "00-josh".into(),
            espn_id: None,
            full_name: "Josh Allen".into(),
            first_name: Some("Josh".into()),
            last_name: Some("Allen".into()),
            position: Some("QB".into()),
            latest_team: Some(TeamAbbr("BUF".into())),
            headshot_url: None,
        }],
        &[Game {
            gsis_game_id: "2026_01_BUF_NYJ".into(),
            season: Season(2026),
            week: Week(1),
            season_type: SeasonType::Reg,
            kickoff: Some(datetime!(2026-09-10 00:00 UTC)),
            home_team: TeamAbbr("NYJ".into()),
            away_team: TeamAbbr("BUF".into()),
            home_score: None,
            away_score: None,
        }],
    )
    .await
    .unwrap();
    let id = SalaryImportService::new(&nfl)
        .upload(csv.as_bytes(), Some("salaries.csv"))
        .await
        .unwrap();
    let app = Router::new().nest("/nfl-data-admin", router::<()>(Arc::clone(&nfl)));
    (app, nfl, id)
}

async fn call(
    app: Router,
    method: &str,
    uri: &str,
    body: Body,
    headers: &[(&str, &str)],
) -> (StatusCode, String) {
    let mut request = Request::builder().method(method).uri(uri);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let response = app.oneshot(request.body(body).unwrap()).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

#[tokio::test]
async fn salary_upload_redirects_to_nfl_review() {
    let (app, _, _) = setup().await;
    let boundary = "salary-boundary";
    let body = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"salaries.csv\"\r\nContent-Type: text/csv\r\n\r\n{CSV}\r\n--{boundary}--\r\n"
    );
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/nfl-data-admin/salary-imports")
                .header(
                    "content-type",
                    format!("multipart/form-data; boundary={boundary}"),
                )
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert!(
        response.headers()["location"]
            .to_str()
            .unwrap()
            .starts_with("/nfl-data-admin/salary-imports/")
    );
}

#[tokio::test]
async fn salary_review_groups_use_nfl_urls() {
    let (app, _, id) = setup_with_csv(UNMATCHED_CSV).await;
    let (status, html) = call(
        app.clone(),
        "GET",
        &format!("/nfl-data-admin/salary-imports/{id}"),
        Body::empty(),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(html.contains(&format!("/nfl-data-admin/salary-imports/{id}/resolved")));
    assert!(html.contains(&format!("/nfl-data-admin/salary-imports/{id}/skipped")));
    let (status, fragment) = call(
        app,
        "GET",
        &format!("/nfl-data-admin/salary-imports/{id}/triage"),
        Body::empty(),
        &[("HX-Request", "true")],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(!fragment.contains("<!doctype html>"));
    assert!(fragment.contains("Unknown Player"));
}

#[tokio::test]
async fn salary_candidate_search_is_lazy_and_scoped() {
    let (app, nfl, id) = setup_with_csv(UNMATCHED_CSV).await;
    let (status, html) = call(
        app.clone(),
        "GET",
        &format!("/nfl-data-admin/salary-imports/{id}"),
        Body::empty(),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(!html.contains("00-josh"));
    assert!(html.contains(&format!("/nfl-data-admin/salary-imports/{id}/rows/")));
    let row = store::rows_for(nfl.managed.reader(), id)
        .await
        .unwrap()
        .remove(0);
    let (status, candidates) = call(
        app,
        "GET",
        &format!(
            "/nfl-data-admin/salary-imports/{id}/rows/{}/players?q=J",
            row.id
        ),
        Body::empty(),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(candidates.contains("00-josh"));
    assert!(
        candidates.contains("on BUF, no weekly roster synced"),
        "{candidates}"
    );
}

#[tokio::test]
async fn forged_salary_mutations_return_not_found_conflict_and_validation() {
    let (app, nfl, id) = setup_with_csv(UNMATCHED_CSV).await;
    let missing = call(
        app.clone(),
        "POST",
        "/nfl-data-admin/salary-imports/999/commit",
        Body::empty(),
        &[],
    )
    .await;
    assert_eq!(missing.0, StatusCode::NOT_FOUND);
    let row = store::rows_for(nfl.managed.reader(), id)
        .await
        .unwrap()
        .remove(0);
    let invalid = call(
        app.clone(),
        "POST",
        &format!("/nfl-data-admin/salary-imports/{id}/rows/{}", row.id),
        Body::from("action=resolve&player_id=unknown"),
        &[("content-type", "application/x-www-form-urlencoded")],
    )
    .await;
    assert_eq!(invalid.0, StatusCode::SEE_OTHER);
    SalaryImportService::new(&nfl).discard(id).await.unwrap();
    let absent = call(
        app,
        "POST",
        &format!("/nfl-data-admin/salary-imports/{id}/discard"),
        Body::empty(),
        &[],
    )
    .await;
    assert_eq!(absent.0, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn forged_salary_mutation_storage_failures_are_generic() {
    let (app, nfl, id) = setup().await;
    nfl.managed
        .write_tx(async |connection| {
            sqlx::query("DROP TABLE salary_import_rows")
                .execute(&mut *connection)
                .await?;
            Ok::<_, sqlx::Error>(())
        })
        .await
        .unwrap();
    let (status, body) = call(
        app,
        "POST",
        &format!("/nfl-data-admin/salary-imports/{id}/commit"),
        Body::empty(),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(body, "Internal server error");
}

#[tokio::test]
async fn committed_review_is_read_only() {
    let (app, nfl, id) = setup().await;
    let service = SalaryImportService::new(&nfl);
    service.commit(id).await.unwrap();
    let row = store::rows_for(nfl.managed.reader(), id)
        .await
        .unwrap()
        .into_iter()
        .find(|row| row.state == RowState::Matched)
        .unwrap();
    let (status, html) = call(
        app.clone(),
        "GET",
        &format!("/nfl-data-admin/salary-imports/{id}"),
        Body::empty(),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(!html.contains(&format!("/rows/{}/undo", row.id)));
    assert!(!html.contains(&format!("/rows/{}/players", row.id)));
    assert!(!html.contains(&format!("/salary-imports/{id}/commit")));
    assert!(!html.contains(&format!("/salary-imports/{id}/discard")));
    let mutation = call(
        app,
        "POST",
        &format!("/nfl-data-admin/salary-imports/{id}/rows/{}/undo", row.id),
        Body::empty(),
        &[],
    )
    .await;
    assert_eq!(mutation.0, StatusCode::CONFLICT);
}
