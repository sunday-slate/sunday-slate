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
const TWO_UNMATCHED_CSV: &str = "\"Id\",\"Position\",\"First Name\",\"Nickname\",\"Last Name\",\"FPPG\",\"Played\",\"Salary\",\"Game\",\"Team\",\"Opponent\",\"Injury Indicator\",\"Injury Details\"\n\"1-101\",\"QB\",\"Unknown\",\"\",\"Player\",\"0\",\"0\",\"8000\",\"BUF@NYJ\",\"BUF\",\"\",\"\",\"\"\n\"1-102\",\"QB\",\"Second\",\"\",\"Player\",\"0\",\"0\",\"8100\",\"BUF@NYJ\",\"BUF\",\"\",\"\",\"\"\n";

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
async fn salary_import_index_uses_html_content_type() {
    let (app, _, _) = setup().await;
    let response = app
        .oneshot(
            Request::builder()
                .uri("/nfl-data-admin/salary-imports")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response.headers()["content-type"]
            .to_str()
            .unwrap()
            .starts_with("text/html")
    );
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let html = String::from_utf8(body.to_vec()).unwrap();
    assert!(html.contains("enctype=\"multipart/form-data\""));
    assert!(html.contains("Recent imports"));
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
async fn missing_salary_import_review_routes_return_not_found_for_full_pages_and_htmx() {
    let (app, _, _) = setup().await;
    let cases = [
        ("/nfl-data-admin/salary-imports/999", false),
        ("/nfl-data-admin/salary-imports/999/resolved", false),
        ("/nfl-data-admin/salary-imports/999/skipped", false),
        ("/nfl-data-admin/salary-imports/999/triage", false),
        ("/nfl-data-admin/salary-imports/999", true),
        ("/nfl-data-admin/salary-imports/999/resolved", true),
        ("/nfl-data-admin/salary-imports/999/skipped", true),
        ("/nfl-data-admin/salary-imports/999/triage", true),
    ];
    let mut observed = Vec::new();
    for (path, is_htmx) in cases {
        let headers: &[(&str, &str)] = if is_htmx {
            &[("HX-Request", "true")]
        } else {
            &[]
        };
        let (status, _) = call(app.clone(), "GET", path, Body::empty(), headers).await;
        observed.push((path, is_htmx, status));
    }
    assert!(
        observed
            .iter()
            .all(|(_, _, status)| *status == StatusCode::NOT_FOUND),
        "missing import response statuses: {observed:?}"
    );
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
async fn htmx_decisions_refresh_counts_and_keep_remaining_controls_actionable() {
    let (app, nfl, id) = setup_with_csv(TWO_UNMATCHED_CSV).await;
    let rows = store::rows_for(nfl.managed.reader(), id).await.unwrap();
    let first = &rows[0];
    let second = &rows[1];
    let (status, skipped) = call(
        app.clone(),
        "POST",
        &format!("/nfl-data-admin/salary-imports/{id}/rows/{}", first.id),
        Body::from("action=skip"),
        &[
            ("HX-Request", "true"),
            ("content-type", "application/x-www-form-urlencoded"),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(skipped.contains("id=\"salary-review-panel\""));
    assert!(skipped.contains("Unmatched: 1"));
    assert!(skipped.contains("Second Player"));
    assert!(skipped.contains(&format!("/salary-imports/{id}/rows/{}/players", second.id)));
    assert!(skipped.contains(&format!(
        "/salary-imports/{id}/rows/{}\" hx-post",
        second.id
    )));

    let (status, undone) = call(
        app,
        "POST",
        &format!("/nfl-data-admin/salary-imports/{id}/rows/{}/undo", first.id),
        Body::empty(),
        &[("HX-Request", "true")],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(undone.contains("Unmatched: 2"));
    assert!(undone.contains("Find canonical player"));
}

#[tokio::test]
async fn widened_short_search_keeps_recovery_form_and_scope() {
    let (app, nfl, id) = setup_with_csv(UNMATCHED_CSV).await;
    let row = store::rows_for(nfl.managed.reader(), id)
        .await
        .unwrap()
        .remove(0);
    let (status, fragment) = call(
        app,
        "GET",
        &format!(
            "/nfl-data-admin/salary-imports/{id}/rows/{}/players?q=J&widened=1",
            row.id
        ),
        Body::empty(),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(fragment.contains("name=\"q\" value=\"J\""), "{fragment}");
    assert!(
        fragment.contains("name=\"widened\" value=\"1\" checked"),
        "{fragment}"
    );
    assert!(fragment.contains("at least two characters"));
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
    assert_eq!(invalid.0, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        invalid
            .1
            .contains("selected player is not an eligible canonical candidate")
    );
    let (_status, commit_feedback) = call(
        app.clone(),
        "GET",
        &format!("/nfl-data-admin/salary-imports/{id}?error=commit_invalid"),
        Body::empty(),
        &[],
    )
    .await;
    assert!(commit_feedback.contains("Resolve or skip every unmatched row and retry."));
    let htmx_invalid = call(
        app.clone(),
        "POST",
        &format!("/nfl-data-admin/salary-imports/{id}/rows/{}", row.id),
        Body::from("action=resolve&player_id=unknown"),
        &[
            ("HX-Request", "true"),
            ("content-type", "application/x-www-form-urlencoded"),
        ],
    )
    .await;
    assert_eq!(htmx_invalid.0, StatusCode::OK);
    assert!(htmx_invalid.1.contains("id=\"salary-review-panel\""));
    assert!(
        htmx_invalid
            .1
            .contains("selected player is not an eligible canonical candidate")
    );
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
async fn invalid_salary_upload_shows_its_diagnostic() {
    let (app, _, _) = setup().await;
    let boundary = "empty-salary-boundary";
    let body = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"empty.csv\"\r\nContent-Type: text/csv\r\n\r\n\r\n--{boundary}--\r\n"
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
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let html = String::from_utf8(bytes.to_vec()).unwrap();
    assert!(html.contains("salary CSV is empty"), "{html}");
}

#[tokio::test]
async fn salary_review_names_selected_and_suggested_roster_only_players() {
    let (app, nfl, id) = setup_with_csv(TWO_UNMATCHED_CSV).await;
    nfl.seed_weekly_roster_for_test(&[crate::WeeklyRosterEntry {
        season: Season(2026),
        week: Week(1),
        team: TeamAbbr("BUF".into()),
        gsis_id: Some("00-roster-only".into()),
        espn_id: None,
        full_name: "Bam Knight".into(),
        last_name: Some("Knight".into()),
        position: Some("RB".into()),
    }])
    .await
    .unwrap();
    let rows = store::rows_for(nfl.managed.reader(), id).await.unwrap();
    let selected_row = &rows[0];
    let fallback_row = &rows[1];
    sqlx::query(
        "UPDATE salary_import_rows SET suggested_gsis_player_id='00-roster-only' WHERE id=?",
    )
    .bind(selected_row.id)
    .execute(nfl.managed.reader())
    .await
    .unwrap();
    sqlx::query("UPDATE salary_import_rows SET suggested_gsis_player_id='00-missing' WHERE id=?")
        .bind(fallback_row.id)
        .execute(nfl.managed.reader())
        .await
        .unwrap();
    SalaryImportService::new(&nfl)
        .resolve(id, selected_row.id, "00-roster-only")
        .await
        .unwrap();

    let (status, full_page) = call(
        app.clone(),
        "GET",
        &format!("/nfl-data-admin/salary-imports/{id}/resolved"),
        Body::empty(),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(full_page.contains("Canonical player: Bam Knight (RB) · 00-roster-only"));
    assert!(full_page.contains("Suggested canonical player: Bam Knight (RB) · 00-roster-only"));

    let (status, fragment) = call(
        app.clone(),
        "GET",
        &format!("/nfl-data-admin/salary-imports/{id}/resolved"),
        Body::empty(),
        &[("HX-Request", "true")],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(!fragment.contains("<!doctype html>"));
    assert!(fragment.contains("Canonical player: Bam Knight (RB) · 00-roster-only"));
    assert!(fragment.contains("Suggested canonical player: Bam Knight (RB) · 00-roster-only"));

    let (status, unmatched) = call(
        app.clone(),
        "GET",
        &format!("/nfl-data-admin/salary-imports/{id}"),
        Body::empty(),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(unmatched.contains("Suggested canonical player: 00-missing"));
    let (status, unmatched_fragment) = call(
        app.clone(),
        "GET",
        &format!("/nfl-data-admin/salary-imports/{id}/triage"),
        Body::empty(),
        &[("HX-Request", "true")],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(unmatched_fragment.contains("Suggested canonical player: 00-missing"));

    SalaryImportService::new(&nfl)
        .skip(id, fallback_row.id)
        .await
        .unwrap();
    SalaryImportService::new(&nfl).commit(id).await.unwrap();
    let (status, committed) = call(
        app,
        "GET",
        &format!("/nfl-data-admin/salary-imports/{id}"),
        Body::empty(),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(committed.contains("Bam Knight (RB) · 00-roster-only"));
}

#[tokio::test]
async fn committed_htmx_groups_do_not_offer_undo() {
    let (app, nfl, id) = setup_with_csv(TWO_UNMATCHED_CSV).await;
    let rows = store::rows_for(nfl.managed.reader(), id).await.unwrap();
    let service = SalaryImportService::new(&nfl);
    service.resolve(id, rows[0].id, "00-josh").await.unwrap();
    service.skip(id, rows[1].id).await.unwrap();
    service.commit(id).await.unwrap();
    for (path, row_id, name) in [
        ("resolved", rows[0].id, "Unknown Player"),
        ("skipped", rows[1].id, "Second Player"),
    ] {
        let (status, fragment) = call(
            app.clone(),
            "GET",
            &format!("/nfl-data-admin/salary-imports/{id}/{path}"),
            Body::empty(),
            &[("HX-Request", "true")],
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(fragment.contains(name), "{fragment}");
        assert!(
            !fragment.contains(&format!("/rows/{row_id}/undo")),
            "{fragment}"
        );
    }
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
