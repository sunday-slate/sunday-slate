use fanduel_data::{FanduelData, FanduelDataConfig, Interpretation, UploadState};

const CSV: &[u8] = include_bytes!("fixtures/salaries.csv");
const CSV_SHA256: &str = "bd5027c18b93104b77731f88ce4235ddffb051b4d1d06eed27bd3c1b85a7d9c8";

#[test]
fn source_store_config_defaults_to_durable_application_path() {
    assert_eq!(
        FanduelDataConfig::default().database_url,
        "sqlite://./storage/fanduel-data.db"
    );
}

#[tokio::test]
async fn archives_exact_blob_name_hash_and_interpreted_source_facts() {
    let service = FanduelData::in_memory().await.unwrap();
    let received = service.receive(CSV, Some("week 1.csv")).await.unwrap();
    let upload = service.upload(received.upload_id).await.unwrap().unwrap();

    assert_eq!(upload.bytes, CSV);
    assert_eq!(upload.metadata.filename.as_deref(), Some("week 1.csv"));
    assert_eq!(upload.metadata.content_hash, CSV_SHA256);
    assert_eq!(upload.metadata.state, UploadState::Parsed);
    let rows = service.source_rows(received.upload_id).await.unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].upload_id, received.upload_id);
    assert_eq!(rows[0].interpreted.row_number, 1);
    assert_eq!(rows[0].interpreted.raw.id, "123506-62239");
    assert_eq!(rows[0].interpreted.raw.fd_player_id(), "62239");
    assert_eq!(rows[0].interpreted.raw.first_name, "Josh");
    assert_eq!(rows[0].interpreted.raw.last_name, "Allen");
    assert_eq!(rows[0].interpreted.raw.salary, 8200);
    assert_eq!(rows[1].interpreted.row_number, 2);
    assert_eq!(rows[1].interpreted.raw.id, "119110-12543");
}

#[tokio::test]
async fn identical_receipts_create_distinct_uploads() {
    let service = FanduelData::in_memory().await.unwrap();
    let first = service.receive(CSV, None).await.unwrap();
    let second = service.receive(CSV, None).await.unwrap();

    assert_ne!(first.upload_id, second.upload_id);
    for id in [first.upload_id, second.upload_id] {
        let upload = service.upload(id).await.unwrap().unwrap();
        assert_eq!(upload.bytes, CSV);
        assert_eq!(upload.metadata.content_hash, CSV_SHA256);
    }
}

#[tokio::test]
async fn source_rows_preserve_original_salary_text_and_numeric_salary() {
    let service = FanduelData::in_memory().await.unwrap();
    let bytes = b"Id,Position,First Name,Nickname,Last Name,FPPG,Played,Salary,Game,Team,Opponent,Injury Indicator,Injury Details\n1,QB,A,,B,0,0,008200,BUF@NYJ,BUF,NYJ,,\n";
    let received = service.receive(bytes, None).await.unwrap();

    let Interpretation::Parsed(rows) = &received.interpretation else {
        panic!("expected parsed upload");
    };
    assert_eq!(rows[0].raw.salary, 8200);
    assert_eq!(rows[0].raw.quoted_salary, "008200");
    let stored = service.source_rows(received.upload_id).await.unwrap();
    assert_eq!(stored[0].interpreted.raw.salary, 8200);
    assert_eq!(stored[0].interpreted.raw.quoted_salary, "008200");
}

#[tokio::test]
async fn hexadecimal_salary_keeps_quoted_value_and_persists_numeric_value() {
    let service = FanduelData::in_memory().await.unwrap();
    let bytes = b"Id,Position,First Name,Nickname,Last Name,FPPG,Played,Salary,Game,Team,Opponent,Injury Indicator,Injury Details\n1,QB,A,,B,0,0,0x2008,BUF@NYJ,BUF,NYJ,,\n";
    let received = service.receive(bytes, None).await.unwrap();
    let Interpretation::Parsed(rows) = &received.interpretation else {
        panic!("expected hexadecimal salary to parse");
    };
    assert_eq!(rows[0].raw.salary, 8200);
    assert_eq!(rows[0].raw.quoted_salary, "0x2008");

    let stored = service.source_rows(received.upload_id).await.unwrap();
    assert_eq!(stored[0].interpreted.raw.salary, 8200);
    assert_eq!(stored[0].interpreted.raw.quoted_salary, "0x2008");
}

#[tokio::test]
async fn raw_row_exposes_optional_composite_list_id() {
    let service = FanduelData::in_memory().await.unwrap();
    let received = service.receive(CSV, None).await.unwrap();
    let rows = service.source_rows(received.upload_id).await.unwrap();
    assert_eq!(rows[0].interpreted.raw.fd_list_id(), Some("123506"));

    let bare = service
        .receive(b"Id,Position,First Name,Nickname,Last Name,FPPG,Played,Salary,Game,Team,Opponent,Injury Indicator,Injury Details\n62239,QB,A,,B,0,0,8200,BUF@NYJ,BUF,NYJ,,\n", None)
        .await
        .unwrap();
    let rows = service.source_rows(bare.upload_id).await.unwrap();
    assert_eq!(rows[0].interpreted.raw.fd_list_id(), None);
}

#[tokio::test]
async fn source_diagnostic_messages_retain_player_name_after_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let config = FanduelDataConfig {
        database_url: format!("sqlite://{}", directory.path().join("fanduel.db").display()),
    };
    let bytes = b"Id,Position,First Name,Nickname,Last Name,FPPG,Played,Salary,Game,Team,Opponent,Injury Indicator,Injury Details\n1,CPT,Josh,,Allen,0,0,8200,BUF-NYJ,BUF,NYJ,,\n";
    let upload_id = FanduelData::connect(config.clone())
        .await
        .unwrap()
        .receive(bytes, None)
        .await
        .unwrap()
        .upload_id;
    let rows = FanduelData::connect(config)
        .await
        .unwrap()
        .source_rows(upload_id)
        .await
        .unwrap();
    assert!(
        rows[0].interpreted.diagnostics[0]
            .message
            .contains("Josh Allen")
    );
    assert!(
        rows[0].interpreted.diagnostics[1]
            .message
            .contains("Josh Allen")
    );
    assert!(rows[0].interpreted.diagnostics[0].message.contains("CPT"));
    assert!(
        rows[0].interpreted.diagnostics[1]
            .message
            .contains("BUF-NYJ")
    );
}

#[tokio::test]
async fn header_only_receipt_is_parsed_with_no_source_rows() {
    let service = FanduelData::in_memory().await.unwrap();
    let bytes = b"Id,Position,First Name,Nickname,Last Name,FPPG,Played,Salary,Game,Team,Opponent,Injury Indicator,Injury Details\n";
    let received = service.receive(bytes, None).await.unwrap();

    assert_eq!(received.interpretation, Interpretation::Empty);
    let upload = service.upload(received.upload_id).await.unwrap().unwrap();
    assert_eq!(upload.metadata.state, UploadState::Parsed);
    assert!(
        service
            .source_rows(received.upload_id)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn empty_receipt_is_parsed_with_no_source_rows() {
    let service = FanduelData::in_memory().await.unwrap();
    let received = service.receive(b"", None).await.unwrap();

    assert_eq!(received.interpretation, Interpretation::Empty);
    let upload = service.upload(received.upload_id).await.unwrap().unwrap();
    assert_eq!(upload.metadata.state, UploadState::Parsed);
    assert!(
        service
            .source_rows(received.upload_id)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn file_backed_upload_and_rows_survive_reconnect() {
    let directory = tempfile::tempdir().unwrap();
    let database_url = format!("sqlite://{}", directory.path().join("fanduel.db").display());
    let config = FanduelDataConfig { database_url };
    let upload_id = {
        let service = FanduelData::connect(config.clone()).await.unwrap();
        service
            .receive(CSV, Some("archive.csv"))
            .await
            .unwrap()
            .upload_id
    };

    let reopened = FanduelData::connect(config).await.unwrap();
    let upload = reopened.upload(upload_id).await.unwrap().unwrap();
    assert_eq!(upload.bytes, CSV);
    assert_eq!(upload.metadata.filename.as_deref(), Some("archive.csv"));
    assert_eq!(upload.metadata.content_hash, CSV_SHA256);
    let rows = reopened.source_rows(upload_id).await.unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[1].interpreted.raw.fd_player_id(), "12543");
}
