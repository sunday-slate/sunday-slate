use fanduel_data::{FanduelData, FanduelDataConfig, Interpretation, UploadState};
use sqlx::sqlite::SqlitePoolOptions;

const HEADER: &str = "Id,Position,First Name,Nickname,Last Name,FPPG,Played,Salary,Game,Team,Opponent,Injury Indicator,Injury Details";

fn config(directory: &tempfile::TempDir) -> FanduelDataConfig {
    FanduelDataConfig {
        database_url: format!("sqlite://{}", directory.path().join("fanduel.db").display()),
    }
}

#[tokio::test]
async fn malformed_later_record_keeps_blob_failed_without_partial_rows() {
    let directory = tempfile::tempdir().unwrap();
    let service = FanduelData::connect(config(&directory)).await.unwrap();
    let bytes = format!(
        "{HEADER}\n1,QB,Josh,,Allen,0,0,8200,BUF@NYJ,BUF,NYJ,,\n2,RB,James,,Cook,0,0,bad,BUF@NYJ,BUF,NYJ,,\n"
    );
    let received = service
        .receive(bytes.as_bytes(), Some("bad-salary.csv"))
        .await
        .unwrap();

    assert!(matches!(
        received.interpretation,
        Interpretation::InvalidCsv(_)
    ));
    let upload = service.upload(received.upload_id).await.unwrap().unwrap();
    assert_eq!(upload.bytes, bytes.as_bytes());
    assert_eq!(upload.metadata.state, UploadState::Failed);
    assert_eq!(
        upload
            .metadata
            .diagnostic
            .as_ref()
            .unwrap()
            .field
            .as_deref(),
        Some("Salary")
    );
    assert!(
        service
            .source_rows(received.upload_id)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn multiline_records_keep_data_record_ordinals() {
    let service = FanduelData::in_memory().await.unwrap();
    let bytes = format!(
        "{HEADER}\n1,QB,Josh,,\"Allen\nJr.\",0,0,8200,BUF@NYJ,BUF,NYJ,,\n2,RB,James,,Cook,0,0,6000,BUF@NYJ,BUF,NYJ,,\n"
    );
    let received = service.receive(bytes.as_bytes(), None).await.unwrap();
    let rows = service.source_rows(received.upload_id).await.unwrap();
    assert_eq!(
        rows.iter()
            .map(|row| row.interpreted.row_number)
            .collect::<Vec<_>>(),
        [1, 2]
    );
}

#[tokio::test]
async fn row_diagnostics_survive_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let config = config(&directory);
    let bytes = format!("{HEADER}\n1,CPT,A,,B,0,0,1,BUF-NYJ,BUF,NYJ,,\n");
    let upload_id = {
        let service = FanduelData::connect(config.clone()).await.unwrap();
        service
            .receive(bytes.as_bytes(), None)
            .await
            .unwrap()
            .upload_id
    };
    let reopened = FanduelData::connect(config).await.unwrap();
    let rows = reopened.source_rows(upload_id).await.unwrap();
    assert_eq!(rows[0].interpreted.diagnostics.len(), 2);
    assert_eq!(rows[0].interpreted.diagnostics[0].field, "Position");
    assert_eq!(rows[0].interpreted.diagnostics[0].row_number, 1);
}

#[tokio::test]
async fn failed_receipt_stops_before_interpretation_and_archiving() {
    let directory = tempfile::tempdir().unwrap();
    let service_config = config(&directory);
    let service = FanduelData::connect(service_config.clone()).await.unwrap();
    let pool = SqlitePoolOptions::new()
        .connect(&service_config.database_url)
        .await
        .unwrap();
    sqlx::query("CREATE TRIGGER reject_upload BEFORE INSERT ON uploads BEGIN SELECT RAISE(ABORT, 'receipt rejected'); END")
        .execute(&pool)
        .await
        .unwrap();
    let result = service.receive(b"", None).await;
    assert!(result.is_err());
    assert!(service.recent_uploads(10).await.unwrap().is_empty());
}

#[tokio::test]
async fn row_insert_failure_rolls_back_complete_set_and_keeps_received_blob() {
    let directory = tempfile::tempdir().unwrap();
    let service_config = config(&directory);
    let service = FanduelData::connect(service_config.clone()).await.unwrap();
    let pool = SqlitePoolOptions::new()
        .connect(&service_config.database_url)
        .await
        .unwrap();
    sqlx::query("CREATE TRIGGER reject_source BEFORE INSERT ON source_rows WHEN NEW.row_number = 2 BEGIN SELECT RAISE(ABORT, 'source rejected'); END")
        .execute(&pool).await.unwrap();
    let bytes = include_bytes!("fixtures/salaries.csv");

    assert!(service.receive(bytes, Some("triggered.csv")).await.is_err());
    let uploads = service.recent_uploads(10).await.unwrap();
    assert_eq!(uploads.len(), 1);
    assert_eq!(uploads[0].state, UploadState::Received);
    let reopened = FanduelData::connect(service_config).await.unwrap();
    let upload = reopened.upload(uploads[0].id).await.unwrap().unwrap();
    assert_eq!(upload.bytes, bytes);
    assert!(
        reopened
            .source_rows(upload.metadata.id)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn failed_diagnostic_update_surfaces_storage_error_and_keeps_bytes() {
    let directory = tempfile::tempdir().unwrap();
    let service_config = config(&directory);
    let service = FanduelData::connect(service_config.clone()).await.unwrap();
    let pool = SqlitePoolOptions::new()
        .connect(&service_config.database_url)
        .await
        .unwrap();
    sqlx::query("CREATE TRIGGER reject_failed_update BEFORE UPDATE ON uploads WHEN NEW.state = 'failed' BEGIN SELECT RAISE(ABORT, 'failure update rejected'); END")
        .execute(&pool).await.unwrap();
    let bytes = format!("{HEADER}\n1,QB,A,,B,0,0,bad,BUF@NYJ,BUF,NYJ,,\n");

    assert!(service.receive(bytes.as_bytes(), None).await.is_err());
    let uploads = service.recent_uploads(10).await.unwrap();
    assert_eq!(uploads.len(), 1);
    assert_eq!(uploads[0].state, UploadState::Received);
    let upload = service.upload(uploads[0].id).await.unwrap().unwrap();
    assert_eq!(upload.bytes, bytes.as_bytes());
}
