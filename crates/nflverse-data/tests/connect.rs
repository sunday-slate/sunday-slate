use nflverse_data::{NflverseData, NflverseDataConfig};

#[tokio::test]
async fn connect_creates_database_and_schema() {
    let dir = tempfile::tempdir().unwrap();
    let config = NflverseDataConfig {
        database_url: format!("sqlite://{}/nflverse-data.db", dir.path().display()),
        ..Default::default()
    };

    let _nfl = NflverseData::connect(config.clone()).await.unwrap();

    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE '_sqlx%' ORDER BY name",
    )
    .fetch_all(&sqlx::SqlitePool::connect(&config.database_url).await.unwrap())
    .await
    .unwrap();
    assert_eq!(
        tables,
        [
            "games",
            "player_week_stats",
            "players",
            "roster_entries",
            "sync_state",
            "team_week_stats",
            "weekly_roster_entries"
        ]
    );
}

#[tokio::test]
async fn in_memory_is_empty_and_queryable() {
    let nfl = nflverse_data::NflverseData::in_memory()
        .await
        .expect("in_memory");
    assert!(
        nfl.games(nflverse_data::Season(2025))
            .await
            .expect("games")
            .is_empty()
    );
    assert!(nfl.players().await.expect("players").is_empty());
}

#[tokio::test]
async fn reopening_preserves_cached_records_and_runs_only_one_baseline() {
    let dir = tempfile::tempdir().unwrap();
    let config = NflverseDataConfig {
        database_url: format!("sqlite://{}/cache.db", dir.path().display()),
        ..Default::default()
    };
    let nfl = NflverseData::connect(config.clone()).await.unwrap();
    let pool = sqlx::SqlitePool::connect(&config.database_url)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO players (gsis_id, full_name, espn_id) VALUES ('00-P', 'Player', '42')",
    )
    .execute(&pool)
    .await
    .unwrap();
    drop(nfl);
    let reopened = NflverseData::connect(config).await.unwrap();
    let players = reopened.players().await.unwrap();
    assert_eq!(players.len(), 1);
    assert_eq!(players[0].espn_id.as_deref(), Some("42"));
    let versions: Vec<i64> = sqlx::query_scalar("SELECT version FROM _sqlx_migrations")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(versions, [20261005000000]);
}

#[tokio::test]
async fn baseline_contains_final_columns_defaults_and_indexes_and_can_be_reversed() {
    let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
    sqlx::raw_sql(include_str!(
        "../migrations/20261005000000_create_nflverse_cache.up.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    let columns: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT name, dflt_value FROM pragma_table_info('player_week_stats')")
            .fetch_all(&pool)
            .await
            .unwrap();
    for name in ["fumble_recovery_tds", "completions", "attempts"] {
        assert!(columns.contains(&(name.to_string(), Some("0".to_string()))));
    }
    for table in ["players", "weekly_roster_entries"] {
        let columns: Vec<String> = sqlx::query_scalar("SELECT name FROM pragma_table_info(?)")
            .bind(table)
            .fetch_all(&pool)
            .await
            .unwrap();
        assert!(columns.iter().any(|name| name == "espn_id"));
    }
    let indexes: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type = 'index' AND name NOT LIKE 'sqlite_%' ORDER BY name"
    ).fetch_all(&pool).await.unwrap();
    assert_eq!(
        indexes,
        [
            "idx_games_season_week",
            "idx_player_week_stats_season_week",
            "idx_roster_entries_season_team",
            "idx_team_week_stats_season_week",
            "idx_weekly_roster_entries_gsis",
            "idx_weekly_roster_entries_season_week_team",
        ]
    );
    sqlx::raw_sql(include_str!(
        "../migrations/20261005000000_create_nflverse_cache.down.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    let tables: i64 = sqlx::query_scalar("SELECT count(*) FROM sqlite_master WHERE type = 'table'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(tables, 0);
}

#[tokio::test]
async fn legacy_schema_is_rejected_without_resetting_it() {
    let dir = tempfile::tempdir().unwrap();
    let config = NflverseDataConfig {
        database_url: format!("sqlite://{}/legacy.db", dir.path().display()),
        ..Default::default()
    };
    let options = config
        .database_url
        .parse::<sqlx::sqlite::SqliteConnectOptions>()
        .unwrap()
        .create_if_missing(true);
    let pool = sqlx::SqlitePool::connect_with(options).await.unwrap();
    sqlx::raw_sql("CREATE TABLE players (gsis_id TEXT, full_name TEXT); INSERT INTO players VALUES ('00-P', 'Legacy');")
        .execute(&pool).await.unwrap();
    assert!(NflverseData::connect(config).await.is_err());
    let name: String = sqlx::query_scalar("SELECT full_name FROM players WHERE gsis_id = '00-P'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(name, "Legacy");
}
