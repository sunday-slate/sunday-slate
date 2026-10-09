use super::service::SalaryImportService;
use crate::{Game, NflData, Player, Season, SeasonType, TeamAbbr, Week, WeeklyRosterEntry};
use time::macros::datetime;

const HEADER: &str = "\"Id\",\"Position\",\"First Name\",\"Nickname\",\"Last Name\",\"FPPG\",\"Played\",\"Salary\",\"Game\",\"Team\",\"Opponent\",\"Injury Indicator\",\"Injury Details\"";

fn csv(rows: &[(&str, &str, &str, &str, &str, &str)]) -> Vec<u8> {
    let mut result = format!("{HEADER}\n");
    for (id, pos, first, last, salary, team) in rows {
        result.push_str(&format!("\"{id}\",\"{pos}\",\"{first}\",\"\",\"{last}\",\"0\",\"0\",\"{salary}\",\"BUF@NYJ\",\"{team}\",\"\",\"\",\"\"\n"));
    }
    result.into_bytes()
}

async fn nfl_with_roster() -> NflData {
    let nfl = NflData::in_memory().await.unwrap();
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
    nfl.seed_weekly_roster_for_test(&[WeeklyRosterEntry {
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
    nfl
}

#[tokio::test]
async fn upload_archives_and_stages_without_price_changes() {
    let nfl = nfl_with_roster().await;
    let service = SalaryImportService::new(&nfl);
    let bytes = csv(&[("1-100", "QB", "Josh", "Allen", "8000", "BUF")]);
    let import_id = service.upload(&bytes, Some("salaries.csv")).await.unwrap();
    assert!(
        nfl.salaries_for_games(&["2026_01_BUF_NYJ"])
            .await
            .unwrap()
            .is_empty()
    );
    let uploads = nfl.fanduel.recent_uploads(10).await.unwrap();
    assert_eq!(uploads.len(), 1);
    assert!(uploads[0].id > 0);
    let rows = super::store::rows_for(nfl.managed.reader(), import_id)
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].state, super::model::RowState::Matched);
    assert_eq!(rows[0].gsis_player_id.as_deref(), Some("00-josh"));
    let precommit: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM confirmed_player_mappings WHERE fd_player_id='100'",
    )
    .fetch_one(nfl.managed.reader())
    .await
    .unwrap();
    assert_eq!(precommit, 0);
    service.commit(import_id).await.unwrap();
    assert!(service.commit(import_id).await.is_err());
    let mapping: String = sqlx::query_scalar(
        "SELECT gsis_player_id FROM confirmed_player_mappings WHERE fd_player_id='100'",
    )
    .fetch_one(nfl.managed.reader())
    .await
    .unwrap();
    assert_eq!(mapping, "00-josh");
}

#[tokio::test]
async fn pending_review_changes_do_not_remember_mappings() {
    let nfl = nfl_with_roster().await;
    let service = SalaryImportService::new(&nfl);
    let bytes = csv(&[("1-200", "WR", "Unknown", "Player", "5100", "BUF")]);
    let import_id = service.upload(&bytes, None).await.unwrap();
    let rows = super::store::rows_for(nfl.managed.reader(), import_id)
        .await
        .unwrap();
    let row = &rows[0];
    assert!(
        service
            .resolve(import_id, row.id, "unknown-canonical-id")
            .await
            .is_err()
    );
    assert!(
        service
            .resolve(import_id, row.id + 999, "00-roster-only")
            .await
            .is_err()
    );
    assert!(service.skip(import_id + 999, row.id).await.is_err());
    service
        .resolve(import_id, row.id, "00-roster-only")
        .await
        .unwrap();
    let mapping: Option<String> = sqlx::query_scalar(
        "SELECT gsis_player_id FROM confirmed_player_mappings WHERE fd_player_id = '200'",
    )
    .fetch_optional(nfl.managed.reader())
    .await
    .unwrap();
    assert_eq!(mapping, None);
    service.skip(import_id, row.id).await.unwrap();
    service.undo(import_id, row.id).await.unwrap();
    let mapping: Option<String> = sqlx::query_scalar(
        "SELECT gsis_player_id FROM confirmed_player_mappings WHERE fd_player_id = '200'",
    )
    .fetch_optional(nfl.managed.reader())
    .await
    .unwrap();
    assert_eq!(mapping, None);
}

#[tokio::test]
async fn automatic_matched_and_defense_rows_reject_manual_decisions() {
    let nfl = nfl_with_roster().await;
    let service = SalaryImportService::new(&nfl);
    let mut bytes =
        String::from_utf8(csv(&[("1-250", "QB", "Josh", "Allen", "8000", "BUF")])).unwrap();
    bytes.push_str("\"1-251\",\"D\",\"New York\",\"\",\"Jets\",\"0\",\"0\",\"3000\",\"BUF@NYJ\",\"NYJ\",\"BUF\",\"\",\"\"\n");
    let import_id = service.upload(bytes.as_bytes(), None).await.unwrap();
    let rows = super::store::rows_for(nfl.managed.reader(), import_id)
        .await
        .unwrap();
    assert_eq!(rows.len(), 2);
    for row in rows {
        assert!(service.skip(import_id, row.id).await.is_err());
        assert!(service.undo(import_id, row.id).await.is_err());
    }
    let persisted = super::store::rows_for(nfl.managed.reader(), import_id)
        .await
        .unwrap();
    assert!(
        persisted
            .iter()
            .any(|row| row.state == super::model::RowState::Matched)
    );
    assert!(
        persisted
            .iter()
            .any(|row| row.state == super::model::RowState::Dst)
    );
}

#[tokio::test]
async fn missing_schedule_facts_are_an_availability_error() {
    let nfl = NflData::in_memory().await.unwrap();
    let service = SalaryImportService::new(&nfl);
    let error = service
        .upload(
            &csv(&[("1-350", "QB", "Josh", "Allen", "8000", "BUF")]),
            None,
        )
        .await
        .unwrap_err();
    assert!(matches!(error, super::service::ImportError::Unavailable(_)));
    assert!(
        super::store::list_recent(nfl.managed.reader(), 10)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn incomplete_commit_is_rejected() {
    let nfl = nfl_with_roster().await;
    let service = SalaryImportService::new(&nfl);
    let import_id = service
        .upload(
            &csv(&[("1-301", "WR", "Unknown", "Player", "5100", "BUF")]),
            None,
        )
        .await
        .unwrap();
    assert!(service.commit(import_id).await.is_err());
    assert_eq!(
        super::store::get(nfl.managed.reader(), import_id)
            .await
            .unwrap()
            .unwrap()
            .status,
        "pending"
    );
}

#[tokio::test]
async fn conflicting_resolution_rolls_back_commit() {
    let nfl = nfl_with_roster().await;
    let service = SalaryImportService::new(&nfl);
    let import_id = service
        .upload(
            &csv(&[("1-401", "WR", "Unknown", "A", "5100", "BUF")]),
            None,
        )
        .await
        .unwrap();
    let rows = super::store::rows_for(nfl.managed.reader(), import_id)
        .await
        .unwrap();
    nfl.managed.write_tx(async |conn| {
        sqlx::query("INSERT INTO confirmed_player_mappings(fd_player_id,gsis_player_id,created_at,updated_at) VALUES('unrelated','00-roster-only','now','now')").execute(&mut *conn).await?;
        Ok::<_, sqlx::Error>(())
    }).await.unwrap();
    service
        .resolve(import_id, rows[0].id, "00-roster-only")
        .await
        .unwrap();
    let result = service.commit(import_id).await;
    assert!(result.is_err());
    assert_eq!(
        super::store::get(nfl.managed.reader(), import_id)
            .await
            .unwrap()
            .unwrap()
            .status,
        "pending"
    );
    assert!(
        nfl.salaries_for_games(&["2026_01_BUF_NYJ"])
            .await
            .unwrap()
            .is_empty()
    );
    let mappings: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM confirmed_player_mappings WHERE fd_player_id='unrelated'",
    )
    .fetch_one(nfl.managed.reader())
    .await
    .unwrap();
    assert_eq!(mappings, 1);
}

#[tokio::test]
async fn duplicate_salary_subject_rolls_back_commit() {
    let nfl = nfl_with_roster().await;
    let service = SalaryImportService::new(&nfl);
    let import_id = service
        .upload(
            &csv(&[
                ("1-501", "WR", "Unknown", "A", "5100", "BUF"),
                ("1-502", "WR", "Unknown", "B", "5200", "BUF"),
            ]),
            None,
        )
        .await
        .unwrap();
    let rows = super::store::rows_for(nfl.managed.reader(), import_id)
        .await
        .unwrap();
    service
        .resolve(import_id, rows[0].id, "00-roster-only")
        .await
        .unwrap();
    service
        .resolve(import_id, rows[1].id, "00-roster-only")
        .await
        .unwrap();
    assert!(service.commit(import_id).await.is_err());
    assert!(
        nfl.salaries_for_games(&["2026_01_BUF_NYJ"])
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        super::store::get(nfl.managed.reader(), import_id)
            .await
            .unwrap()
            .unwrap()
            .status,
        "pending"
    );
}

#[tokio::test]
async fn concurrent_decision_cannot_mutate_committed_review() {
    let nfl = nfl_with_roster().await;
    let service = SalaryImportService::new(&nfl);
    let import_id = service
        .upload(
            &csv(&[("1-601", "QB", "Josh", "Allen", "8000", "BUF")]),
            None,
        )
        .await
        .unwrap();
    let row = super::store::rows_for(nfl.managed.reader(), import_id)
        .await
        .unwrap()[0]
        .clone();
    let (committed, decided) =
        tokio::join!(service.commit(import_id), service.skip(import_id, row.id));
    assert!(committed.is_ok());
    let _ = decided;
    assert!(service.skip(import_id, row.id).await.is_err());
    let fresh = super::store::rows_for(nfl.managed.reader(), import_id)
        .await
        .unwrap();
    assert_eq!(fresh[0].state, super::model::RowState::Matched);
    assert_eq!(
        super::store::get(nfl.managed.reader(), import_id)
            .await
            .unwrap()
            .unwrap()
            .status,
        "committed"
    );
}

#[tokio::test]
async fn rejected_archive_does_not_change_committed_provenance_domain() {
    let nfl = nfl_with_roster().await;
    let service = SalaryImportService::new(&nfl);
    let rejected = String::from_utf8(csv(&[("1-701", "QB", "Josh", "Allen", "8000", "BUF")]))
        .unwrap()
        .replace("BUF@NYJ", "BUF-NYJ");
    assert!(service.upload(rejected.as_bytes(), None).await.is_err());
    let import_id = service
        .upload(
            &csv(&[("1-702", "QB", "Josh", "Allen", "8000", "BUF")]),
            None,
        )
        .await
        .unwrap();
    let rows = super::store::rows_for(nfl.managed.reader(), import_id)
        .await
        .unwrap();
    let local_row_id = rows[0].id;
    let batch = super::store::get(nfl.managed.reader(), import_id)
        .await
        .unwrap()
        .unwrap();
    let provider_rows = nfl
        .fanduel
        .source_rows(batch.source_upload_id)
        .await
        .unwrap();
    let provider_row_id = provider_rows[0].id;
    assert_ne!(local_row_id, provider_row_id);
    service.commit(import_id).await.unwrap();
    let provenance: i64 = sqlx::query_scalar(
        "SELECT source_row_id FROM current_salaries WHERE gsis_game_id='2026_01_BUF_NYJ'",
    )
    .fetch_one(nfl.managed.reader())
    .await
    .unwrap();
    assert_eq!(provenance, local_row_id);
}

#[tokio::test]
async fn cache_loss_preserves_managed_and_archive_data() {
    use crate::NflDataConfig;
    let directory = tempfile::tempdir().unwrap();
    let cache_path = directory.path().join("cache.db");
    let managed_path = directory.path().join("managed.db");
    let archive_path = directory.path().join("archive.db");
    let config = NflDataConfig {
        database_url: format!("sqlite://{}", cache_path.display()),
        managed_database_url: format!("sqlite://{}", managed_path.display()),
        fanduel_database_url: format!("sqlite://{}", archive_path.display()),
        ..Default::default()
    };
    let nfl = NflData::connect(config.clone()).await.unwrap();
    nfl.seed_for_test(
        &[Player {
            gsis_id: "00-josh".into(),
            espn_id: None,
            full_name: "Josh Allen".into(),
            first_name: None,
            last_name: None,
            position: Some("QB".into()),
            latest_team: Some(TeamAbbr("BUF".into())),
            headshot_url: None,
        }],
        &[Game {
            gsis_game_id: "2026_01_BUF_NYJ".into(),
            season: Season(2026),
            week: Week(1),
            season_type: SeasonType::Reg,
            kickoff: None,
            home_team: TeamAbbr("NYJ".into()),
            away_team: TeamAbbr("BUF".into()),
            home_score: None,
            away_score: None,
        }],
    )
    .await
    .unwrap();
    let service = SalaryImportService::new(&nfl);
    let import_id = service
        .upload(
            &csv(&[("1-801", "QB", "Josh", "Allen", "8000", "BUF")]),
            Some("durable.csv"),
        )
        .await
        .unwrap();
    service.commit(import_id).await.unwrap();
    drop(service);
    drop(nfl);
    std::fs::remove_file(&cache_path).unwrap();
    let reopened = NflData::connect(config).await.unwrap();
    assert_eq!(
        reopened
            .salaries_for_games(&["2026_01_BUF_NYJ"])
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(reopened.fanduel.recent_uploads(10).await.unwrap().len(), 1);
}

#[tokio::test]
async fn skipped_rows_leave_existing_prices_and_mappings_unchanged() {
    let nfl = nfl_with_roster().await;
    nfl.managed.write_tx(async |conn| {
        sqlx::query("INSERT INTO current_salaries(gsis_game_id,gsis_player_id,team_abbr,dfs_position,salary,provenance,created_at,updated_at) VALUES('2026_01_BUF_NYJ','00-josh','BUF','QB',8200,'legacy','now','now')")
            .execute(&mut *conn).await?;
        Ok::<_, sqlx::Error>(())
    }).await.unwrap();
    let service = SalaryImportService::new(&nfl);
    let import_id = service
        .upload(
            &csv(&[("1-850", "WR", "Unknown", "Player", "5100", "BUF")]),
            None,
        )
        .await
        .unwrap();
    let row = super::store::rows_for(nfl.managed.reader(), import_id)
        .await
        .unwrap()[0]
        .clone();
    service.skip(import_id, row.id).await.unwrap();
    let report = service.commit(import_id).await.unwrap();
    assert_eq!(report.salaries, 0);
    let prices = nfl.salaries_for_games(&["2026_01_BUF_NYJ"]).await.unwrap();
    assert_eq!(prices.len(), 1);
    assert_eq!(prices[0].salary, 8200);
    let mappings: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM confirmed_player_mappings")
        .fetch_one(nfl.managed.reader())
        .await
        .unwrap();
    assert_eq!(mappings, 0);
}

#[tokio::test]
async fn discard_removes_pending_review_but_preserves_provider_archive() {
    let nfl = nfl_with_roster().await;
    let service = SalaryImportService::new(&nfl);
    let import_id = service
        .upload(
            &csv(&[("1-901", "WR", "Unknown", "Player", "5100", "BUF")]),
            Some("keep.csv"),
        )
        .await
        .unwrap();
    let batch = super::store::get(nfl.managed.reader(), import_id)
        .await
        .unwrap()
        .unwrap();
    service.discard(import_id).await.unwrap();
    assert!(
        super::store::get(nfl.managed.reader(), import_id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        nfl.fanduel
            .upload(batch.source_upload_id)
            .await
            .unwrap()
            .is_some()
    );
    assert!(service.discard(import_id).await.is_err());
}

#[tokio::test]
async fn roster_only_suggestions_require_confirmation() {
    let nfl = nfl_with_roster().await;
    let service = SalaryImportService::new(&nfl);
    let bytes = csv(&[("1-201", "RB", "Zonovan", "Knight", "5000", "BUF")]);
    let import_id = service.upload(&bytes, None).await.unwrap();
    let rows = super::store::rows_for(nfl.managed.reader(), import_id)
        .await
        .unwrap();
    assert_eq!(rows[0].state, super::model::RowState::Unmatched);
    assert_eq!(
        rows[0].suggested_gsis_player_id.as_deref(),
        Some("00-roster-only")
    );
    assert!(
        nfl.salaries_for_games(&["2026_01_BUF_NYJ"])
            .await
            .unwrap()
            .is_empty()
    );
}
