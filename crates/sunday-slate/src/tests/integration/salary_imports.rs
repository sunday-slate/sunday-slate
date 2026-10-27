#[tokio::test]
async fn migrations_remove_the_staged_import_tables() {
    let app = crate::tests::TestApp::new().await;
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type = 'table' \
         AND name IN ('salary_import_rows', 'salary_imports') ORDER BY name",
    )
    .fetch_all(&app.pool)
    .await
    .unwrap();
    assert!(tables.is_empty(), "staged tables remain: {tables:?}");
}

#[tokio::test]
async fn cutover_migration_preserves_players_and_drops_salary_storage() {
    // Built from migration FILES up to 20261014000000 (the pre-cutover schema
    // with the fd_player_id column), then the new 20261026010000 migration is
    // applied on top. This proves the boundary: local player rows, GSIS ids,
    // timestamps, and headshot references survive; the obsolete salary table
    // and the fd_player_id column disappear; deleting a media row still nulls
    // the headshot reference.
    let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
    sqlx::raw_sql(include_str!(
        "../../../migrations/20260729120000_create_player_data.up.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::raw_sql(include_str!(
        "../../../migrations/20260809120000_create_media.up.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::raw_sql(include_str!(
        "../../../migrations/20260824000000_headshot_cache.up.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::raw_sql(include_str!(
        "../../../migrations/20261014000000_fd_player_id_integer.up.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO nfl_players (gsis_player_id, fd_player_id) VALUES ('gsis-preserved', 12345)",
    )
    .execute(&pool)
    .await
    .unwrap();
    let media_id: i64 = sqlx::query_scalar(
        "INSERT INTO media (path, content_type, byte_size, width, height) \
         VALUES ('headshots/preserved.png', 'image/png', 12, 256, 128) RETURNING id",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE nfl_players SET headshot_media_id = ?1 WHERE gsis_player_id = 'gsis-preserved'",
    )
    .bind(media_id)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO nfl_player_salaries (gsis_game_id, gsis_player_id, team_abbr, dfs_position, salary) VALUES ('game-preserved', 'gsis-preserved', 'BUF', 'QB', 8000)")
        .execute(&pool)
        .await
        .unwrap();

    let (created, updated): (String, String) = sqlx::query_as(
        "SELECT created_at, updated_at FROM nfl_players WHERE gsis_player_id = 'gsis-preserved'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();

    sqlx::raw_sql(include_str!(
        "../../../migrations/20261026010000_remove_salary_data.up.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();

    // Local player identity, timestamps, and headshot reference survive.
    let (id, gsis, headshot): (i64, String, Option<i64>) =
        sqlx::query_as("SELECT id, gsis_player_id, headshot_media_id FROM nfl_players")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(gsis, "gsis-preserved");
    assert_eq!(headshot, Some(media_id));
    let (created_after, updated_after): (String, String) =
        sqlx::query_as("SELECT created_at, updated_at FROM nfl_players WHERE id = ?1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(created_after, created);
    assert_eq!(updated_after, updated);

    // Obsolete salary storage and the FD column are gone.
    let salary_table: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='nfl_player_salaries'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(salary_table, 0);
    let has_fd_column: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pragma_table_info('nfl_players') WHERE name = 'fd_player_id'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(has_fd_column, 0);

    // Deleting the media row still nulls the headshot reference.
    sqlx::query("DELETE FROM media WHERE id = ?1")
        .bind(media_id)
        .execute(&pool)
        .await
        .unwrap();
    let headshot_after_delete: Option<i64> =
        sqlx::query_scalar("SELECT headshot_media_id FROM nfl_players WHERE id = ?1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(headshot_after_delete, None);

    // Rollback restores the pre-cutover schema only (schema, not values).
    let previous = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
    sqlx::raw_sql(include_str!(
        "../../../migrations/20260729120000_create_player_data.up.sql"
    ))
    .execute(&previous)
    .await
    .unwrap();
    sqlx::raw_sql(include_str!(
        "../../../migrations/20260809120000_create_media.up.sql"
    ))
    .execute(&previous)
    .await
    .unwrap();
    sqlx::raw_sql(include_str!(
        "../../../migrations/20260824000000_headshot_cache.up.sql"
    ))
    .execute(&previous)
    .await
    .unwrap();
    sqlx::raw_sql(include_str!(
        "../../../migrations/20261014000000_fd_player_id_integer.up.sql"
    ))
    .execute(&previous)
    .await
    .unwrap();
    sqlx::raw_sql(include_str!(
        "../../../migrations/20261026010000_remove_salary_data.down.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    type Column = (i64, String, String, i64, Option<String>, i64);
    let before: Vec<Column> = sqlx::query_as("PRAGMA table_info(nfl_players)")
        .fetch_all(&previous)
        .await
        .unwrap();
    let restored: Vec<Column> = sqlx::query_as("PRAGMA table_info(nfl_players)")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(
        before, restored,
        "down migration restores the prior player table shape"
    );
}
