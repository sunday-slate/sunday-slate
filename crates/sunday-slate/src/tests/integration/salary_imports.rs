use crate::admin::salary_imports::schedule::resolve_slate_games;
use nfl_data::{Game, Season, SeasonType, SeasonWeek, Week};
use std::collections::BTreeSet;
use time::OffsetDateTime;
use time::macros::datetime;

fn game(id: &str, week: u8, kickoff: OffsetDateTime) -> Game {
    Game {
        gsis_game_id: id.into(),
        season: Season(2025),
        week: Week(week),
        season_type: SeasonType::Reg,
        kickoff: Some(kickoff),
        home_team: "SEA".parse().unwrap(),
        away_team: "SF".parse().unwrap(),
        home_score: None,
        away_score: None,
    }
}

#[tokio::test]
async fn migrations_remove_the_staged_import_tables() {
    let app = crate::tests::TestApp::new().await;
    let tables: Vec<String> = sqlx::query_scalar("SELECT name FROM sqlite_master WHERE type = 'table' AND name IN ('salary_import_rows', 'salary_imports') ORDER BY name")
        .fetch_all(&app.pool)
        .await
        .unwrap();
    assert!(tables.is_empty(), "staged tables remain: {tables:?}");
}

#[test]
fn schedule_resolution_requires_complete_season_week() {
    let games = vec![
        game("reg", 1, datetime!(2025-09-07 17:00 UTC)),
        game("post", 20, datetime!(2026-01-18 17:00 UTC)),
    ];
    let matchups = BTreeSet::from([("SF".to_owned(), "SEA".to_owned())]);
    let regular = SeasonWeek {
        season: Season(2025),
        season_type: SeasonType::Reg,
        week: Week(1),
    };
    let postseason = SeasonWeek {
        season: Season(2025),
        season_type: SeasonType::Post,
        week: Week(20),
    };
    assert_eq!(
        resolve_slate_games(&games, &regular, &matchups).unwrap()[0].gsis_game_id,
        "reg"
    );
    assert!(resolve_slate_games(&games, &postseason, &matchups).is_err());
}

#[tokio::test]
async fn workflow_migration_rollback_recreates_the_previous_schema_and_index() {
    let pool = crate::tests::utils::in_memory_pool().await;
    sqlx::query(
        "INSERT INTO nfl_players (gsis_player_id, fd_player_id) VALUES ('gsis-preserved', 12345)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO nfl_player_salaries (gsis_game_id, gsis_player_id, team_abbr, dfs_position, salary) VALUES ('game-preserved', 'gsis-preserved', 'BUF', 'QB', 8000)")
        .execute(&pool).await.unwrap();
    let previous = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
    sqlx::raw_sql("CREATE TABLE users (id INTEGER PRIMARY KEY);")
        .execute(&previous)
        .await
        .unwrap();
    sqlx::raw_sql(include_str!(
        "../../../migrations/20260801150000_create_salary_imports.up.sql"
    ))
    .execute(&previous)
    .await
    .unwrap();
    sqlx::raw_sql(include_str!(
        "../../../migrations/20261013000000_drop_salary_import_workflow.down.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    for (table, columns_query, foreign_keys_query) in [
        (
            "salary_imports",
            "PRAGMA table_info(salary_imports)",
            "PRAGMA foreign_key_list(salary_imports)",
        ),
        (
            "salary_import_rows",
            "PRAGMA table_info(salary_import_rows)",
            "PRAGMA foreign_key_list(salary_import_rows)",
        ),
    ] {
        type Column = (i64, String, String, i64, Option<String>, i64);
        let before: Vec<Column> = sqlx::query_as(columns_query)
            .fetch_all(&previous)
            .await
            .unwrap();
        let restored: Vec<Column> = sqlx::query_as(columns_query)
            .fetch_all(&pool)
            .await
            .unwrap();
        assert_eq!(before, restored, "column definitions for {table}");
        type ForeignKey = (i64, i64, String, String, String, String, String, String);
        let before: Vec<ForeignKey> = sqlx::query_as(foreign_keys_query)
            .fetch_all(&previous)
            .await
            .unwrap();
        let restored: Vec<ForeignKey> = sqlx::query_as(foreign_keys_query)
            .fetch_all(&pool)
            .await
            .unwrap();
        assert_eq!(before, restored, "foreign keys for {table}");
    }
    let index: Vec<(i64, i64, String)> =
        sqlx::query_as("PRAGMA index_info(ix_salary_import_rows_import)")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(index, vec![(0, 1, "import_id".into())]);
    sqlx::query("INSERT INTO salary_imports (id, season) VALUES (1, 2025)")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO salary_import_rows (import_id, fd_player_id, fd_name, fd_team, dfs_position, salary, gsis_game_id, state) VALUES (1, '1', 'Player', 'BUF', 'QB', 8000, 'game', 'unmatched')")
        .execute(&pool).await.unwrap();
    sqlx::raw_sql(include_str!(
        "../../../migrations/20261013000000_drop_salary_import_workflow.up.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    let preserved: i64 = sqlx::query_scalar("SELECT count(*) FROM sqlite_master WHERE type='table' AND name IN ('nfl_players', 'nfl_player_salaries')")
        .fetch_one(&pool).await.unwrap();
    assert_eq!(preserved, 2);
    let salary: i64 = sqlx::query_scalar(
        "SELECT salary FROM nfl_player_salaries WHERE gsis_game_id = 'game-preserved'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(salary, 8000);
    let fd: u32 = sqlx::query_scalar(
        "SELECT fd_player_id FROM nfl_players WHERE gsis_player_id = 'gsis-preserved'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(fd, 12345);
}
