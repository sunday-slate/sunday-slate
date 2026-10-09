use nfl_data::{DfsPosition, NflData, NflDataConfig, Salary, Season, TeamAbbr};
use sqlx::{
    SqlitePool,
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
};
use std::str::FromStr;
use tempfile::tempdir;

async fn nfl() -> (tempfile::TempDir, NflData, SqlitePool, SqlitePool) {
    let dir = tempdir().unwrap();
    let config = NflDataConfig {
        database_url: format!("sqlite://{}", dir.path().join("cache.db").display()),
        fanduel_database_url: format!("sqlite://{}", dir.path().join("archive.db").display()),
        managed_database_url: format!("sqlite://{}", dir.path().join("managed.db").display()),
        managed_season: Season(2025),
        ..Default::default()
    };
    let nfl = NflData::connect(config).await.unwrap();
    let legacy_options = SqliteConnectOptions::from_str(&format!(
        "sqlite://{}",
        dir.path().join("legacy.db").display()
    ))
    .unwrap()
    .create_if_missing(true);
    let legacy = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(legacy_options)
        .await
        .unwrap();
    let managed = SqlitePoolOptions::new()
        .max_connections(1)
        .connect(&format!(
            "sqlite://{}",
            dir.path().join("managed.db").display()
        ))
        .await
        .unwrap();
    (dir, nfl, legacy, managed)
}

async fn legacy_schema(db: &SqlitePool) {
    sqlx::query("CREATE TABLE nfl_player_salaries (id INTEGER PRIMARY KEY, gsis_game_id TEXT NOT NULL, gsis_player_id TEXT, team_abbr TEXT NOT NULL, dfs_position TEXT NOT NULL, salary INTEGER NOT NULL, created_at TEXT NOT NULL, updated_at TEXT NOT NULL)")
        .execute(db).await.unwrap();
    sqlx::query("CREATE TABLE media (id INTEGER PRIMARY KEY, path TEXT UNIQUE, content_type TEXT NOT NULL, byte_size INTEGER NOT NULL, width INTEGER NOT NULL, height INTEGER NOT NULL, created_at TEXT NOT NULL, updated_at TEXT NOT NULL)")
        .execute(db).await.unwrap();
    sqlx::query("CREATE TABLE nfl_players (id INTEGER PRIMARY KEY, gsis_player_id TEXT NOT NULL UNIQUE, fd_player_id TEXT UNIQUE, created_at TEXT NOT NULL, updated_at TEXT NOT NULL, headshot_media_id INTEGER REFERENCES media(id) ON DELETE SET NULL)")
        .execute(db).await.unwrap();
    sqlx::query("CREATE TABLE salary_imports (id INTEGER PRIMARY KEY, status TEXT NOT NULL)")
        .execute(db)
        .await
        .unwrap();
}

#[tokio::test]
async fn migrates_legacy_player_and_defense_prices() {
    let (_dir, nfl, legacy, managed) = nfl().await;
    legacy_schema(&legacy).await;
    sqlx::query("INSERT INTO nfl_player_salaries VALUES (1, '2025_01_BUF_NYJ', '00-1', 'BUF', 'QB', 8200, '2025-08-01T01:02:03Z', '2025-08-02T01:02:03Z'), (2, '2025_01_BUF_NYJ', NULL, 'NYJ', 'DST', 4000, '2025-08-01T02:02:03Z', '2025-08-02T02:02:03Z')")
        .execute(&legacy).await.unwrap();
    sqlx::query("INSERT INTO media VALUES (1, 'players/00-1.webp', 'image/webp', 100, 80, 80, 'media-created', 'media-updated')")
        .execute(&legacy).await.unwrap();
    sqlx::query("INSERT INTO nfl_players VALUES (1, '00-1', '62239', '2025-07-01T01:02:03Z', '2025-07-02T01:02:03Z', 1)")
        .execute(&legacy).await.unwrap();
    nfl.migrate_legacy_salaries(&legacy).await.unwrap();
    assert_eq!(
        nfl.salaries_for_games(&["2025_01_BUF_NYJ"]).await.unwrap(),
        vec![
            Salary {
                gsis_game_id: "2025_01_BUF_NYJ".into(),
                gsis_player_id: Some("00-1".into()),
                team_abbr: TeamAbbr("BUF".into()),
                dfs_position: DfsPosition::Qb,
                salary: 8200
            },
            Salary {
                gsis_game_id: "2025_01_BUF_NYJ".into(),
                gsis_player_id: None,
                team_abbr: TeamAbbr("NYJ".into()),
                dfs_position: DfsPosition::Dst,
                salary: 4000
            },
        ]
    );
    let stored = sqlx::query_as::<_, (String, String, String)>(
        "SELECT created_at, updated_at, provenance FROM current_salaries ORDER BY dfs_position",
    )
    .fetch_all(&managed)
    .await
    .unwrap();
    assert_eq!(
        stored[0],
        (
            "2025-08-01T02:02:03Z".into(),
            "2025-08-02T02:02:03Z".into(),
            "legacy".into()
        )
    );
    assert_eq!(
        stored[1],
        (
            "2025-08-01T01:02:03Z".into(),
            "2025-08-02T01:02:03Z".into(),
            "legacy".into()
        )
    );
    let mapping: (String, String, String) = sqlx::query_as("SELECT gsis_player_id, created_at, updated_at FROM confirmed_player_mappings WHERE fd_player_id = '62239'").fetch_one(&managed).await.unwrap();
    assert_eq!(
        mapping,
        (
            "00-1".into(),
            "2025-07-01T01:02:03Z".into(),
            "2025-07-02T01:02:03Z".into()
        )
    );
    let source_salaries: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM nfl_player_salaries")
        .fetch_one(&legacy)
        .await
        .unwrap();
    let source_players: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM nfl_players")
        .fetch_one(&legacy)
        .await
        .unwrap();
    assert_eq!((source_salaries, source_players), (2, 1));
    let association: Option<i64> = sqlx::query_scalar(
        "SELECT headshot_media_id FROM nfl_players WHERE gsis_player_id = '00-1'",
    )
    .fetch_one(&legacy)
    .await
    .unwrap();
    let media: (String, String, String) =
        sqlx::query_as("SELECT path, created_at, updated_at FROM media WHERE id = 1")
            .fetch_one(&legacy)
            .await
            .unwrap();
    assert_eq!(association, Some(1));
    assert_eq!(
        media,
        (
            "players/00-1.webp".into(),
            "media-created".into(),
            "media-updated".into()
        )
    );
}

#[tokio::test]
async fn rejects_pending_legacy_reviews() {
    let (_dir, nfl, legacy, managed) = nfl().await;
    legacy_schema(&legacy).await;
    sqlx::query("INSERT INTO salary_imports VALUES (1, 'pending')")
        .execute(&legacy)
        .await
        .unwrap();
    assert!(nfl.migrate_legacy_salaries(&legacy).await.is_err());
    let marker: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM migration_state")
        .fetch_one(&managed)
        .await
        .unwrap();
    let salaries: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM current_salaries")
        .fetch_one(&managed)
        .await
        .unwrap();
    let mappings: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM confirmed_player_mappings")
        .fetch_one(&managed)
        .await
        .unwrap();
    assert_eq!((marker, salaries, mappings), (0, 0, 0));
}

#[tokio::test]
async fn migration_failure_rolls_back_all_target_facts_and_can_retry() {
    let (_dir, nfl, legacy, managed) = nfl().await;
    legacy_schema(&legacy).await;
    sqlx::query("INSERT INTO nfl_player_salaries VALUES (1, '2025_01_BUF_NYJ', '00-1', 'BUF', 'QB', 8200, 'created', 'updated')")
        .execute(&legacy).await.unwrap();
    sqlx::query("INSERT INTO nfl_players VALUES (1, '00-1', '62239', 'created', 'updated', NULL)")
        .execute(&legacy)
        .await
        .unwrap();
    sqlx::query("INSERT INTO confirmed_player_mappings VALUES ('62239', '00-other', 'old', 'old')")
        .execute(&managed)
        .await
        .unwrap();
    assert!(nfl.migrate_legacy_salaries(&legacy).await.is_err());
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM current_salaries")
        .fetch_one(&managed)
        .await
        .unwrap();
    let marker: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM migration_state")
        .fetch_one(&managed)
        .await
        .unwrap();
    assert_eq!((count, marker), (0, 0));
    sqlx::query("DELETE FROM confirmed_player_mappings")
        .execute(&managed)
        .await
        .unwrap();
    nfl.migrate_legacy_salaries(&legacy).await.unwrap();
    assert_eq!(
        nfl.salaries_for_games(&["2025_01_BUF_NYJ"]).await.unwrap()[0].salary,
        8200
    );
}

#[tokio::test]
async fn conflicting_legacy_price_aborts_without_marker() {
    let (_dir, nfl, legacy, managed) = nfl().await;
    legacy_schema(&legacy).await;
    sqlx::query("INSERT INTO nfl_player_salaries VALUES (1, '2025_01_BUF_NYJ', '00-1', 'BUF', 'QB', 8200, 'created', 'updated')")
        .execute(&legacy).await.unwrap();
    sqlx::query("INSERT INTO current_salaries(gsis_game_id, gsis_player_id, team_abbr, dfs_position, salary, provenance, created_at, updated_at) VALUES ('2025_01_BUF_NYJ', '00-1', 'BUF', 'QB', 8100, 'legacy', 'old', 'old')")
        .execute(&managed).await.unwrap();
    assert!(nfl.migrate_legacy_salaries(&legacy).await.is_err());
    let marker: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM migration_state")
        .fetch_one(&managed)
        .await
        .unwrap();
    assert_eq!(marker, 0);
}

#[tokio::test]
async fn reverse_mapping_conflict_rolls_back_copied_salaries_and_new_mappings() {
    let (_dir, nfl, legacy, managed) = nfl().await;
    legacy_schema(&legacy).await;
    sqlx::query("INSERT INTO nfl_player_salaries VALUES (1, '2025_01_BUF_NYJ', '00-1', 'BUF', 'QB', 8200, 'created', 'updated')")
        .execute(&legacy).await.unwrap();
    sqlx::query("INSERT INTO nfl_players VALUES (1, '00-new', '60000', 'new-created', 'new-updated', NULL), (2, '00-1', '62239', 'source-created', 'source-updated', NULL)")
        .execute(&legacy).await.unwrap();
    sqlx::query("INSERT INTO confirmed_player_mappings VALUES ('old-fd-id', '00-1', 'existing-created', 'existing-updated')")
        .execute(&managed).await.unwrap();

    assert!(nfl.migrate_legacy_salaries(&legacy).await.is_err());

    let salary_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM current_salaries")
        .fetch_one(&managed)
        .await
        .unwrap();
    let marker_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM migration_state")
        .fetch_one(&managed)
        .await
        .unwrap();
    let mappings: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT fd_player_id, gsis_player_id, created_at FROM confirmed_player_mappings ORDER BY fd_player_id",
    ).fetch_all(&managed).await.unwrap();
    assert_eq!(salary_count, 0);
    assert_eq!(marker_count, 0);
    assert_eq!(
        mappings,
        vec![("old-fd-id".into(), "00-1".into(), "existing-created".into())]
    );
}

#[tokio::test]
async fn guarded_migration_entrypoint_migrates_empty_database() {
    let dir = tempdir().unwrap();
    let url = format!("sqlite://{}", dir.path().join("managed.db").display());
    nfl_data::migrate_managed_database(&url).await.unwrap();
    let db = SqlitePoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .unwrap();
    let tables: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name IN ('current_salaries', 'salary_imports', 'salary_import_rows')",
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(tables, 3);
}

#[tokio::test]
async fn guarded_migration_entrypoint_rejects_unrelated_database_without_reset() {
    let dir = tempdir().unwrap();
    let url = format!("sqlite://{}", dir.path().join("managed.db").display());
    let db = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::from_str(&url)
                .unwrap()
                .create_if_missing(true),
        )
        .await
        .unwrap();
    sqlx::query("CREATE TABLE keep_me (value TEXT NOT NULL)")
        .execute(&db)
        .await
        .unwrap();
    sqlx::query("INSERT INTO keep_me VALUES ('preserve')")
        .execute(&db)
        .await
        .unwrap();
    assert!(nfl_data::migrate_managed_database(&url).await.is_err());
    let preserved: String = sqlx::query_scalar("SELECT value FROM keep_me")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(preserved, "preserve");
}
#[tokio::test]
async fn empty_migration_history_can_be_retried() {
    let dir = tempdir().unwrap();
    let url = format!("sqlite://{}", dir.path().join("managed.db").display());
    let db = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::from_str(&url)
                .unwrap()
                .create_if_missing(true),
        )
        .await
        .unwrap();
    sqlx::query("CREATE TABLE _sqlx_migrations (version BIGINT PRIMARY KEY, description TEXT NOT NULL, installed_on TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP, success BOOLEAN NOT NULL, checksum BLOB NOT NULL, execution_time BIGINT NOT NULL)")
        .execute(&db)
        .await
        .unwrap();
    db.close().await;

    let config = NflDataConfig {
        database_url: format!("sqlite://{}", dir.path().join("cache.db").display()),
        fanduel_database_url: format!("sqlite://{}", dir.path().join("archive.db").display()),
        managed_database_url: url.clone(),
        ..Default::default()
    };
    NflData::connect(config).await.unwrap();

    let migrated = SqlitePoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .unwrap();
    for table in [
        "current_salaries",
        "confirmed_player_mappings",
        "salary_imports",
        "salary_import_rows",
        "migration_state",
    ] {
        let exists: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name = ?",
        )
        .bind(table)
        .fetch_one(&migrated)
        .await
        .unwrap();
        assert_eq!(exists, 1, "missing migrated table {table}");
    }
}
#[tokio::test]
async fn unrelated_existing_database_is_rejected_without_reset() {
    let dir = tempdir().unwrap();
    let old_db = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::from_str(&format!(
                "sqlite://{}",
                dir.path().join("unrelated.db").display()
            ))
            .unwrap()
            .create_if_missing(true),
        )
        .await
        .unwrap();
    sqlx::query("CREATE TABLE keep_me (value TEXT NOT NULL)")
        .execute(&old_db)
        .await
        .unwrap();
    sqlx::query("INSERT INTO keep_me VALUES ('preserve')")
        .execute(&old_db)
        .await
        .unwrap();
    let error = match NflData::connect(NflDataConfig {
        database_url: format!("sqlite://{}", dir.path().join("cache.db").display()),
        fanduel_database_url: format!("sqlite://{}", dir.path().join("archive.db").display()),
        managed_database_url: format!("sqlite://{}", dir.path().join("unrelated.db").display()),
        ..Default::default()
    })
    .await
    {
        Ok(_) => panic!("unrelated managed database unexpectedly opened"),
        Err(error) => error,
    };
    assert!(matches!(error, nfl_data::NflDataConnectError::Managed(_)));
    let preserved: String = sqlx::query_scalar("SELECT value FROM keep_me")
        .fetch_one(&old_db)
        .await
        .unwrap();
    assert_eq!(preserved, "preserve");
}

#[tokio::test]
async fn completed_migration_does_not_overwrite_later_prices() {
    let (_dir, nfl, legacy, managed) = nfl().await;
    legacy_schema(&legacy).await;
    sqlx::query("INSERT INTO nfl_player_salaries VALUES (1, '2025_01_BUF_NYJ', '00-1', 'BUF', 'QB', 8200, 'created', 'updated')")
        .execute(&legacy).await.unwrap();
    nfl.migrate_legacy_salaries(&legacy).await.unwrap();
    sqlx::query("UPDATE current_salaries SET salary = 9000 WHERE gsis_game_id = '2025_01_BUF_NYJ'")
        .execute(&managed)
        .await
        .unwrap();
    nfl.migrate_legacy_salaries(&legacy).await.unwrap();
    assert_eq!(
        nfl.salaries_for_games(&["2025_01_BUF_NYJ"]).await.unwrap()[0].salary,
        9000
    );
}

#[tokio::test]
async fn concurrent_migrations_record_one_marker_without_replaying_facts() {
    let (dir, first, legacy, managed) = nfl().await;
    legacy_schema(&legacy).await;
    sqlx::query("INSERT INTO nfl_player_salaries VALUES (1, '2025_01_BUF_NYJ', '00-1', 'BUF', 'QB', 8200, 'created', 'updated')")
        .execute(&legacy).await.unwrap();
    let second = NflData::connect(NflDataConfig {
        database_url: format!("sqlite://{}", dir.path().join("cache.db").display()),
        fanduel_database_url: format!("sqlite://{}", dir.path().join("archive.db").display()),
        managed_database_url: format!("sqlite://{}", dir.path().join("managed.db").display()),
        managed_season: Season(2025),
        ..Default::default()
    })
    .await
    .unwrap();
    let (a, b) = tokio::join!(
        first.migrate_legacy_salaries(&legacy),
        second.migrate_legacy_salaries(&legacy)
    );
    a.unwrap();
    b.unwrap();
    let markers: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM migration_state WHERE key = 'sunday-slate-salaries-v1'",
    )
    .fetch_one(&managed)
    .await
    .unwrap();
    let salaries: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM current_salaries")
        .fetch_one(&managed)
        .await
        .unwrap();
    assert_eq!((markers, salaries), (1, 1));
}

#[tokio::test]
async fn salary_reads_deduplicate_games_and_order_ties() {
    let (_dir, nfl, _legacy, _managed) = nfl().await;
    let rows = [
        Salary {
            gsis_game_id: "2025_01_BUF_NYJ".into(),
            gsis_player_id: Some("00-2".into()),
            team_abbr: TeamAbbr("BUF".into()),
            dfs_position: DfsPosition::Rb,
            salary: 5000,
        },
        Salary {
            gsis_game_id: "2025_01_BUF_NYJ".into(),
            gsis_player_id: Some("00-1".into()),
            team_abbr: TeamAbbr("BUF".into()),
            dfs_position: DfsPosition::Qb,
            salary: 5000,
        },
    ];
    for row in &rows {
        let position = match row.dfs_position {
            DfsPosition::Qb => "QB",
            DfsPosition::Rb => "RB",
            DfsPosition::Wr => "WR",
            DfsPosition::Te => "TE",
            DfsPosition::Dst => "DST",
        };
        sqlx::query("INSERT INTO current_salaries(gsis_game_id, gsis_player_id, team_abbr, dfs_position, salary, provenance, created_at, updated_at) VALUES (?, ?, ?, ?, ?, 'legacy', '2025-01-01', '2025-01-01')")
            .bind(&row.gsis_game_id).bind(&row.gsis_player_id).bind(&row.team_abbr.0).bind(position).bind(row.salary)
            .execute(&_managed).await.unwrap();
    }
    assert!(
        nfl.salaries_for_games::<&str>(&[])
            .await
            .unwrap()
            .is_empty()
    );
    let got = nfl
        .salaries_for_games(&["2025_01_BUF_NYJ", "2025_01_BUF_NYJ"])
        .await
        .unwrap();
    assert_eq!(got, vec![rows[1].clone(), rows[0].clone()]);
}
