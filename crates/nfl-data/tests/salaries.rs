use fanduel_data::{FanduelData, FanduelDataConfig};
use nfl_data::{DfsPosition, NflData, NflDataConfig, SalaryMatchup, SalaryUploadOutcome, TeamAbbr};

const CSV: &[u8] = b"Id,Position,First Name,Nickname,Last Name,FPPG,Played,Salary,Game,Team,Opponent,Injury Indicator,Injury Details\n123506-62239,QB,Josh,,Allen,22,0,8200,BUF@NYJ,BUF,NYJ,,\n";
const INVALID_CSV: &[u8] = b"Id,Position,First Name,Nickname,Last Name,FPPG,Played,Salary,Game,Team,Opponent,Injury Indicator,Injury Details\n123506-62239,QB,Josh,,Allen,22,0,not-a-salary,BUF@NYJ,BUF,NYJ,,\n";
const ROW_DIAGNOSTICS_CSV: &[u8] = b"Id,Position,First Name,Nickname,Last Name,FPPG,Played,Salary,Game,Team,Opponent,Injury Indicator,Injury Details\n123506-62239,CPT,Josh,,Allen,22,0,8200,BUF-NYJ,BUF,NYJ,,\n";

fn config(nflverse: &std::path::Path, fanduel: &std::path::Path) -> NflDataConfig {
    NflDataConfig {
        database_url: format!("sqlite://{}", nflverse.display()),
        fanduel_database_url: format!("sqlite://{}", fanduel.display()),
        ..Default::default()
    }
}

#[tokio::test]
async fn upload_projects_id_free_salary_and_archives_exact_source_facts() {
    let dir = tempfile::tempdir().unwrap();
    let nflverse = dir.path().join("cache.db");
    let archive = dir.path().join("fanduel.db");
    let cfg = config(&nflverse, &archive);
    let nfl = NflData::connect(cfg.clone()).await.unwrap();

    let outcome = nfl
        .receive_salary_upload(CSV, Some("salaries.csv"))
        .await
        .unwrap();
    assert_eq!(
        outcome,
        SalaryUploadOutcome::Parsed(vec![nfl_data::SalaryRow {
            fd_player_id: "62239".into(),
            name: "Josh Allen".into(),
            team: TeamAbbr("BUF".into()),
            original_position: "QB".into(),
            position: Some(DfsPosition::Qb),
            matchup: Some(SalaryMatchup {
                away: TeamAbbr("BUF".into()),
                home: TeamAbbr("NYJ".into()),
            }),
            salary: 8200,
            diagnostics: vec![],
        }])
    );
    drop(nfl);

    let provider = FanduelData::connect(FanduelDataConfig {
        database_url: format!("sqlite://{}", archive.display()),
    })
    .await
    .unwrap();
    let uploads = provider.recent_uploads(10).await.unwrap();
    assert_eq!(uploads.len(), 1);
    assert_eq!(uploads[0].filename.as_deref(), Some("salaries.csv"));
    let upload = provider.upload(uploads[0].id).await.unwrap().unwrap();
    assert_eq!(upload.bytes, CSV);
    let rows = provider.source_rows(uploads[0].id).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].interpreted.raw.name(), "Josh Allen");

    let nfl = NflData::connect(cfg).await.unwrap();
    nfl.receive_salary_upload(CSV, Some("salaries.csv"))
        .await
        .unwrap();
    let uploads = provider.recent_uploads(10).await.unwrap();
    assert_eq!(uploads.len(), 2);
    assert_ne!(uploads[0].id, uploads[1].id);
}

#[tokio::test]
async fn in_memory_facades_keep_fanduel_receipts_independent() {
    let first = NflData::in_memory().await.unwrap();
    let second = NflData::in_memory().await.unwrap();
    first.receive_salary_upload(CSV, None).await.unwrap();
    let second_outcome = second.receive_salary_upload(CSV, None).await.unwrap();
    assert_eq!(
        second_outcome,
        SalaryUploadOutcome::Parsed(vec![nfl_data::SalaryRow {
            fd_player_id: "62239".into(),
            name: "Josh Allen".into(),
            team: TeamAbbr("BUF".into()),
            original_position: "QB".into(),
            position: Some(DfsPosition::Qb),
            matchup: Some(SalaryMatchup {
                away: TeamAbbr("BUF".into()),
                home: TeamAbbr("NYJ".into()),
            }),
            salary: 8200,
            diagnostics: vec![],
        }])
    );
}

#[tokio::test]
async fn facade_preserves_empty_and_malformed_csv_outcomes() {
    let nfl = NflData::in_memory().await.unwrap();
    assert_eq!(
        nfl.receive_salary_upload(b"", None).await.unwrap(),
        SalaryUploadOutcome::Empty
    );
    assert!(matches!(
        nfl.receive_salary_upload(INVALID_CSV, None).await.unwrap(),
        SalaryUploadOutcome::InvalidCsv(_)
    ));
}

#[tokio::test]
async fn facade_row_diagnostics_preserve_source_name_and_original_values() {
    let nfl = NflData::in_memory().await.unwrap();
    let SalaryUploadOutcome::Parsed(rows) = nfl
        .receive_salary_upload(ROW_DIAGNOSTICS_CSV, None)
        .await
        .unwrap()
    else {
        panic!("expected row diagnostics in parsed outcome");
    };
    assert_eq!(rows[0].diagnostics.len(), 2);
    assert!(rows[0].diagnostics[0].message.contains("Josh Allen"));
    assert!(rows[0].diagnostics[0].message.contains("CPT"));
    assert!(rows[0].diagnostics[1].message.contains("Josh Allen"));
    assert!(rows[0].diagnostics[1].message.contains("BUF-NYJ"));
}

#[tokio::test]
async fn fanduel_startup_failure_is_identified_and_nflverse_reads_stay_separate() {
    let dir = tempfile::tempdir().unwrap();
    let error = NflData::connect(config(
        &dir.path().join("cache.db"),
        &dir.path().join("missing/fanduel.db"),
    ))
    .await
    .err()
    .expect("invalid FanDuel path should fail startup");
    assert!(error.to_string().contains("FanDuel"));

    let cache = dir.path().join("cache.db");
    let source = dir.path().join("fanduel.db");
    let cfg = config(&cache, &source);
    let nfl = NflData::connect(cfg.clone()).await.unwrap();
    nfl.receive_salary_upload(CSV, None).await.unwrap();
    drop(nfl);
    std::fs::remove_file(&cache).unwrap();
    let nfl = NflData::connect(cfg).await.unwrap();
    assert!(nfl.games(nfl_data::Season(2025)).await.unwrap().is_empty());
    assert_eq!(
        FanduelData::connect(FanduelDataConfig {
            database_url: format!("sqlite://{}", source.display()),
        })
        .await
        .unwrap()
        .recent_uploads(10)
        .await
        .unwrap()
        .len(),
        1
    );
}
