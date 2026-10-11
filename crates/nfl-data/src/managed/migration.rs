use sqlx::{SqliteConnection, SqlitePool};

use super::{
    error::SalaryError,
    model::{LegacyMapping, LegacySalary},
    store::ManagedStore,
};

const MARKER: &str = "sunday-slate-salaries-v1";

impl ManagedStore {
    pub(crate) async fn migrate_legacy(&self, legacy: &SqlitePool) -> Result<(), SalaryError> {
        let mut source = legacy.begin().await?;
        let has_imports = table_exists(&mut source, "salary_imports").await?;
        if has_imports {
            let pending: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM salary_imports WHERE status = 'pending'")
                    .fetch_one(&mut *source)
                    .await?;
            if pending != 0 {
                return Err(SalaryError::pending_reviews());
            }
        }
        let mut salaries = Vec::new();
        if table_exists(&mut source, "nfl_player_salaries").await? {
            salaries = sqlx::query_as::<_, (String, Option<String>, String, String, i64, String, String)>(
                "SELECT gsis_game_id, gsis_player_id, team_abbr, dfs_position, salary, created_at, updated_at FROM nfl_player_salaries ORDER BY id")
                .fetch_all(&mut *source).await?
                .into_iter().map(|(game_id, player_id, team, position, salary, created_at, updated_at)| LegacySalary {
                    game_id, player_id, team, position, salary, created_at, updated_at,
                }).collect();
        }
        let mut mappings = Vec::new();
        if table_exists(&mut source, "nfl_players").await? {
            mappings = sqlx::query_as::<_, (String, String, String, String)>(
                "SELECT fd_player_id, gsis_player_id, created_at, updated_at FROM nfl_players WHERE fd_player_id IS NOT NULL ORDER BY id")
                .fetch_all(&mut *source).await?
                .into_iter().map(|(fd_player_id, gsis_player_id, created_at, updated_at)| LegacyMapping {
                    fd_player_id, gsis_player_id, created_at, updated_at,
                }).collect();
        }
        source.commit().await?;

        self.write_tx(async |conn| {
            let marker: Option<String> = sqlx::query_scalar("SELECT key FROM migration_state WHERE key = ?")
                .bind(MARKER).fetch_optional(&mut *conn).await?;
            if marker.is_some() { return Ok(()); }
            for salary in &salaries { copy_salary(conn, salary).await?; }
            for mapping in &mappings { copy_mapping(conn, mapping).await?; }
            sqlx::query("INSERT INTO migration_state(key, completed_at) VALUES (?, datetime('now','subsec'))")
                .bind(MARKER).execute(&mut *conn).await?;
            Ok(())
        }).await
    }
}

async fn table_exists(conn: &mut SqliteConnection, table: &str) -> Result<bool, sqlx::Error> {
    Ok(sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?",
    )
    .bind(table)
    .fetch_one(conn)
    .await?
        != 0)
}

async fn copy_salary(
    conn: &mut SqliteConnection,
    salary: &LegacySalary,
) -> Result<(), SalaryError> {
    if (salary.position == "DST") != salary.player_id.is_none() {
        return Err(SalaryError::conflict(
            "legacy salary has invalid player/defense shape",
        ));
    }
    let existing: Option<(Option<String>, String, String, i64)> = if salary.position == "DST" {
        sqlx::query_as("SELECT gsis_player_id, team_abbr, dfs_position, salary FROM current_salaries WHERE gsis_game_id = ? AND dfs_position = 'DST' AND team_abbr = ?")
            .bind(&salary.game_id).bind(&salary.team).fetch_optional(&mut *conn).await?
    } else {
        sqlx::query_as("SELECT gsis_player_id, team_abbr, dfs_position, salary FROM current_salaries WHERE gsis_game_id = ? AND dfs_position <> 'DST' AND gsis_player_id = ?")
            .bind(&salary.game_id).bind(&salary.player_id).fetch_optional(&mut *conn).await?
    };
    if let Some((player, team, position, amount)) = existing {
        if player == salary.player_id
            && team == salary.team
            && position == salary.position
            && amount == salary.salary
        {
            return Ok(());
        }
        return Err(SalaryError::conflict(format!(
            "salary key {} / {:?}",
            salary.game_id, salary.player_id
        )));
    }
    sqlx::query("INSERT INTO current_salaries(gsis_game_id, gsis_player_id, team_abbr, dfs_position, salary, provenance, import_id, source_row_id, created_at, updated_at) VALUES (?, ?, ?, ?, ?, 'legacy', NULL, NULL, ?, ?)")
        .bind(&salary.game_id).bind(&salary.player_id).bind(&salary.team).bind(&salary.position)
        .bind(salary.salary).bind(&salary.created_at).bind(&salary.updated_at).execute(&mut *conn).await?;
    let represented: i64 = if salary.position == "DST" {
        sqlx::query_scalar("SELECT COUNT(*) FROM current_salaries WHERE gsis_game_id = ? AND gsis_player_id IS NULL AND team_abbr = ? AND dfs_position = ? AND salary = ?")
            .bind(&salary.game_id).bind(&salary.team).bind(&salary.position).bind(salary.salary).fetch_one(&mut *conn).await?
    } else {
        sqlx::query_scalar("SELECT COUNT(*) FROM current_salaries WHERE gsis_game_id = ? AND gsis_player_id = ? AND team_abbr = ? AND dfs_position = ? AND salary = ?")
            .bind(&salary.game_id).bind(&salary.player_id).bind(&salary.team).bind(&salary.position).bind(salary.salary).fetch_one(&mut *conn).await?
    };
    if represented != 1 {
        return Err(SalaryError::conflict("legacy salary was not represented"));
    }
    Ok(())
}

async fn copy_mapping(
    conn: &mut SqliteConnection,
    mapping: &LegacyMapping,
) -> Result<(), SalaryError> {
    let by_fd: Option<String> = sqlx::query_scalar(
        "SELECT gsis_player_id FROM confirmed_player_mappings WHERE fd_player_id = ?",
    )
    .bind(&mapping.fd_player_id)
    .fetch_optional(&mut *conn)
    .await?;
    let by_gsis: Option<String> = sqlx::query_scalar(
        "SELECT fd_player_id FROM confirmed_player_mappings WHERE gsis_player_id = ?",
    )
    .bind(&mapping.gsis_player_id)
    .fetch_optional(&mut *conn)
    .await?;
    if let Some(gsis) = by_fd.as_deref()
        && gsis != mapping.gsis_player_id
    {
        return Err(SalaryError::conflict(format!(
            "FanDuel mapping {}",
            mapping.fd_player_id
        )));
    }
    if let Some(fd) = by_gsis.as_deref()
        && fd != mapping.fd_player_id
    {
        return Err(SalaryError::conflict(format!(
            "GSIS mapping {}",
            mapping.gsis_player_id
        )));
    }
    if by_fd.is_none() && by_gsis.is_none() {
        sqlx::query("INSERT INTO confirmed_player_mappings(fd_player_id, gsis_player_id, created_at, updated_at) VALUES (?, ?, ?, ?)")
            .bind(&mapping.fd_player_id).bind(&mapping.gsis_player_id).bind(&mapping.created_at).bind(&mapping.updated_at)
            .execute(&mut *conn).await?;
    }
    let represented: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM confirmed_player_mappings WHERE fd_player_id = ? AND gsis_player_id = ?")
        .bind(&mapping.fd_player_id).bind(&mapping.gsis_player_id).fetch_one(&mut *conn).await?;
    if represented != 1 {
        return Err(SalaryError::conflict("legacy mapping was not represented"));
    }
    Ok(())
}
