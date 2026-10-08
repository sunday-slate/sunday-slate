use axum_test::multipart::{MultipartForm, Part};
use fanduel_data::{FanduelData, FanduelDataConfig};
use http::StatusCode;
use nfl_data::{Game, NflDataConfig, Player, Season, SeasonType, TeamAbbr as NflTeamAbbr, Week};
use std::sync::Arc;

use crate::admin::salary_imports::store as import_store;
use crate::tests::TestApp;
use crate::tests::factories::{self, UserOptions};

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
    Player {
        gsis_id: gsis.into(),
        espn_id: None,
        full_name: name.into(),
        first_name: None,
        last_name: None,
        position: Some("WR".into()),
        latest_team: Some(NflTeamAbbr(team.into())),
        headshot_url: None,
    }
}
const HDR: &str = "\"Id\",\"Position\",\"First Name\",\"Nickname\",\"Last Name\",\"FPPG\",\"Played\",\"Salary\",\"Game\",\"Team\",\"Opponent\",\"Injury Indicator\",\"Injury Details\"";

fn slate_csv() -> String {
    // fd id 200's name won't match the seeded "Jaxon Smith-Njigba".
    format!(
        "{HDR}\n\"1-200\",\"WR\",\"J.\",\"\",\"Smith-Njigba\",\"0\",\"0\",\"5000\",\"BUF@NYJ\",\"BUF\",\"NYJ\",\"\",\"\"\n"
    )
}

async fn isolated_app() -> (tempfile::TempDir, TestApp) {
    let dir = tempfile::tempdir().unwrap();
    let fanduel_url = format!("sqlite://{}", dir.path().join("fanduel.db").display());
    let nfl = Arc::new(
        nfl_data::NflData::connect(NflDataConfig {
            database_url: "sqlite::memory:".into(),
            fanduel_database_url: fanduel_url,
            ..Default::default()
        })
        .await
        .unwrap(),
    );
    let app = TestApp::from_pool_with_nfl(crate::tests::utils::in_memory_pool().await, nfl).await;
    app.login_admin().await;
    (dir, app)
}

async fn archived_uploads(dir: &tempfile::TempDir) -> Vec<fanduel_data::UploadMetadata> {
    FanduelData::connect(FanduelDataConfig {
        database_url: format!("sqlite://{}", dir.path().join("fanduel.db").display()),
    })
    .await
    .unwrap()
    .recent_uploads(20)
    .await
    .unwrap()
}
fn upload_form() -> MultipartForm {
    MultipartForm::new().add_part(
        "csv",
        Part::bytes(slate_csv().into_bytes())
            .file_name("s.csv")
            .mime_type("text/csv"),
    )
}

#[tokio::test]
async fn multipart_upload_retains_filename_and_exact_bytes_once() {
    let dir = tempfile::tempdir().unwrap();
    let fanduel_url = format!("sqlite://{}", dir.path().join("fanduel.db").display());
    let nfl = Arc::new(
        nfl_data::NflData::connect(NflDataConfig {
            database_url: "sqlite::memory:".into(),
            fanduel_database_url: fanduel_url.clone(),
            ..Default::default()
        })
        .await
        .unwrap(),
    );
    let app = TestApp::from_pool_with_nfl(crate::tests::utils::in_memory_pool().await, nfl).await;
    app.login_admin().await;
    app.nfl
        .seed_for_test(&[], &[game("2025_01_BUF_NYJ", "BUF", "NYJ")])
        .await
        .unwrap();

    let csv = slate_csv();
    let form = MultipartForm::new().add_part(
        "csv",
        Part::bytes(csv.as_bytes().to_vec())
            .file_name("week-one-original.csv")
            .mime_type("text/csv"),
    );
    app.server
        .post("/admin/salary-imports")
        .multipart(form)
        .await
        .assert_status(StatusCode::SEE_OTHER);

    let archive = FanduelData::connect(FanduelDataConfig {
        database_url: fanduel_url,
    })
    .await
    .unwrap();
    let uploads = archive.recent_uploads(10).await.unwrap();
    assert_eq!(uploads.len(), 1);
    assert_eq!(
        uploads[0].filename.as_deref(),
        Some("week-one-original.csv")
    );
    let upload = archive.upload(uploads[0].id).await.unwrap().unwrap();
    assert_eq!(upload.bytes, csv.as_bytes());
}

#[tokio::test]
async fn invalid_csv_is_archived_without_staging_an_import() {
    let (dir, app) = isolated_app().await;
    let bytes = b"not,a,fanduel,csv\n";
    let form = MultipartForm::new().add_part(
        "csv",
        Part::bytes(bytes.to_vec())
            .file_name("broken.csv")
            .mime_type("text/csv"),
    );
    app.server
        .post("/admin/salary-imports")
        .multipart(form)
        .await
        .assert_status_ok();
    assert_eq!(
        import_store::list_recent(&app.pool, 10)
            .await
            .unwrap()
            .len(),
        0
    );
    let uploads = archived_uploads(&dir).await;
    assert_eq!(uploads.len(), 1);
    assert_eq!(uploads[0].filename.as_deref(), Some("broken.csv"));
    let archive = FanduelData::connect(FanduelDataConfig {
        database_url: format!("sqlite://{}", dir.path().join("fanduel.db").display()),
    })
    .await
    .unwrap();
    assert_eq!(
        archive.upload(uploads[0].id).await.unwrap().unwrap().bytes,
        bytes
    );
}

#[tokio::test]
async fn unresolvable_slate_retains_upload_without_changing_prices() {
    let (dir, app) = isolated_app().await;
    sqlx::query("INSERT INTO nfl_player_salaries (gsis_game_id, gsis_player_id, team_abbr, dfs_position, salary) VALUES ('old-game', 'old-player', 'BUF', 'WR', 4100)")
        .execute(&app.pool).await.unwrap();
    let form = MultipartForm::new().add_part(
        "csv",
        Part::bytes(slate_csv().into_bytes()).file_name("unknown-week.csv"),
    );
    app.server
        .post("/admin/salary-imports")
        .multipart(form)
        .await
        .assert_status_ok();
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT salary FROM nfl_player_salaries WHERE gsis_game_id = 'old-game'"
        )
        .fetch_one(&app.pool)
        .await
        .unwrap(),
        4100
    );
    assert_eq!(
        import_store::list_recent(&app.pool, 10)
            .await
            .unwrap()
            .len(),
        0
    );
    let uploads = archived_uploads(&dir).await;
    assert_eq!(uploads.len(), 1);
    assert_eq!(uploads[0].filename.as_deref(), Some("unknown-week.csv"));
}

#[tokio::test]
async fn staging_failure_retains_upload_and_leaves_prices_unchanged() {
    let (dir, app) = isolated_app().await;
    app.nfl
        .seed_for_test(&[], &[game("2025_01_BUF_NYJ", "BUF", "NYJ")])
        .await
        .unwrap();
    sqlx::query("CREATE TRIGGER reject_salary_import BEFORE INSERT ON salary_imports BEGIN SELECT RAISE(FAIL, 'staging rejected'); END")
        .execute(&app.pool).await.unwrap();
    let form = MultipartForm::new().add_part(
        "csv",
        Part::bytes(slate_csv().into_bytes()).file_name("stage-fail.csv"),
    );
    app.server
        .post("/admin/salary-imports")
        .multipart(form)
        .await
        .assert_status(StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM nfl_player_salaries")
            .fetch_one(&app.pool)
            .await
            .unwrap(),
        0
    );
    let uploads = archived_uploads(&dir).await;
    assert_eq!(uploads.len(), 1);
    assert_eq!(uploads[0].filename.as_deref(), Some("stage-fail.csv"));
}

#[tokio::test]
async fn discard_does_not_remove_archived_upload() {
    let (dir, app) = isolated_app().await;
    app.nfl
        .seed_for_test(&[], &[game("2025_01_BUF_NYJ", "BUF", "NYJ")])
        .await
        .unwrap();
    let bytes = slate_csv().into_bytes();
    let form = MultipartForm::new().add_part(
        "csv",
        Part::bytes(bytes.clone()).file_name("discard-me.csv"),
    );
    let response = app
        .server
        .post("/admin/salary-imports")
        .multipart(form)
        .await;
    let id = response
        .header("location")
        .to_str()
        .unwrap()
        .rsplit('/')
        .next()
        .unwrap()
        .parse::<i64>()
        .unwrap();
    app.post(&format!("/admin/salary-imports/{id}/discard"), "")
        .await
        .assert_status(StatusCode::SEE_OTHER);
    let uploads = archived_uploads(&dir).await;
    assert_eq!(uploads.len(), 1);
    assert_eq!(uploads[0].filename.as_deref(), Some("discard-me.csv"));
    let archive = FanduelData::connect(FanduelDataConfig {
        database_url: format!("sqlite://{}", dir.path().join("fanduel.db").display()),
    })
    .await
    .unwrap();
    assert_eq!(
        archive.upload(uploads[0].id).await.unwrap().unwrap().bytes,
        bytes
    );
}

#[tokio::test]
async fn salary_import_schema_keeps_existing_linkage_without_archive_references() {
    let app = TestApp::new().await;
    let columns: Vec<String> =
        sqlx::query_scalar("SELECT name FROM pragma_table_info('salary_import_rows') ORDER BY cid")
            .fetch_all(&app.pool)
            .await
            .unwrap();
    assert!(columns.iter().any(|column| column == "import_id"));
    assert!(columns.iter().any(|column| column == "fd_player_id"));
    assert!(columns.iter().any(|column| column == "gsis_game_id"));
    assert!(columns.iter().any(|column| column == "gsis_player_id"));
    assert!(!columns.iter().any(|column| column.contains("upload")));
    assert!(!columns.iter().any(|column| column.contains("source_row")));
}

#[tokio::test]
async fn resolve_then_reimport_auto_matches() {
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
    app.nfl
        .seed_for_test(
            &[player("00-0039337", "Jaxon Smith-Njigba", "BUF")],
            &[game("2025_01_BUF_NYJ", "BUF", "NYJ")],
        )
        .await
        .unwrap();

    // First upload: one unmatched row.
    let loc = app
        .server
        .post("/admin/salary-imports")
        .multipart(upload_form())
        .await
        .header("location")
        .to_str()
        .unwrap()
        .to_string();
    let id: i64 = loc.rsplit('/').next().unwrap().parse().unwrap();
    assert_eq!(
        import_store::unmatched_count(&app.pool, id).await.unwrap(),
        1
    );

    // Resolve it to the real player.
    let row_id = import_store::rows_for(&app.pool, id).await.unwrap()[0].id;
    app.post_htmx(
        &format!("/admin/salary-imports/{id}/rows/{row_id}"),
        "gsis_player_id=00-0039337",
    )
    .await
    .assert_status_ok();

    // Commit.
    app.post(&format!("/admin/salary-imports/{id}/commit"), "")
        .await
        .assert_status(StatusCode::SEE_OTHER);

    // Re-upload the same CSV: the crosswalk now auto-matches — zero unmatched.
    let loc2 = app
        .server
        .post("/admin/salary-imports")
        .multipart(upload_form())
        .await
        .header("location")
        .to_str()
        .unwrap()
        .to_string();
    let id2: i64 = loc2.rsplit('/').next().unwrap().parse().unwrap();
    assert_eq!(
        import_store::unmatched_count(&app.pool, id2).await.unwrap(),
        0,
        "resolution should stick via the crosswalk"
    );
}
