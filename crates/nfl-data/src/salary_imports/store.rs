use super::model::{RowGroup, RowState, SalaryImport, SalaryImportRow, StateCounts};
use nfl_model::DfsPosition;
use sqlx::{SqliteConnection, SqlitePool};

fn position(s: String) -> Result<DfsPosition, sqlx::Error> {
    match s.as_str() {
        "QB" => Ok(DfsPosition::Qb),
        "RB" => Ok(DfsPosition::Rb),
        "WR" => Ok(DfsPosition::Wr),
        "TE" => Ok(DfsPosition::Te),
        "DST" => Ok(DfsPosition::Dst),
        _ => Err(sqlx::Error::Decode(
            format!("unsupported position {s}").into(),
        )),
    }
}
fn state(s: String) -> Result<RowState, sqlx::Error> {
    match s.as_str() {
        "matched" => Ok(RowState::Matched),
        "dst" => Ok(RowState::Dst),
        "unmatched" => Ok(RowState::Unmatched),
        "resolved" => Ok(RowState::Resolved),
        "skipped" => Ok(RowState::Skipped),
        _ => Err(sqlx::Error::Decode(
            format!("unsupported row state {s}").into(),
        )),
    }
}

pub(crate) async fn list_recent(
    pool: &SqlitePool,
    limit: i64,
) -> Result<Vec<SalaryImport>, sqlx::Error> {
    let rows = sqlx::query_as::<_, (i64,i64,i64,String,String,i64,i64,String,Option<String>)>("SELECT id, season, week, season_type, status, games, source_upload_id, received_at, committed_at FROM salary_imports ORDER BY id DESC LIMIT ?")
        .bind(limit).fetch_all(pool).await?;
    Ok(rows
        .into_iter()
        .map(|r| SalaryImport {
            id: r.0,
            season: r.1,
            week: r.2,
            season_type: r.3,
            status: r.4,
            games: r.5,
            source_upload_id: r.6,
            received_at: r.7,
            committed_at: r.8,
        })
        .collect())
}
pub(crate) async fn get(pool: &SqlitePool, id: i64) -> Result<Option<SalaryImport>, sqlx::Error> {
    Ok(sqlx::query_as::<_, (i64,i64,i64,String,String,i64,i64,String,Option<String>)>("SELECT id, season, week, season_type, status, games, source_upload_id, received_at, committed_at FROM salary_imports WHERE id = ?")
        .bind(id).fetch_optional(pool).await?.map(|r| SalaryImport { id:r.0, season:r.1, week:r.2, season_type:r.3, status:r.4, games:r.5, source_upload_id:r.6, received_at:r.7, committed_at:r.8 }))
}
pub(crate) async fn rows_for(
    pool: &SqlitePool,
    import_id: i64,
) -> Result<Vec<SalaryImportRow>, sqlx::Error> {
    let rows = sqlx::query_as::<_, (i64,i64,i64,String,String,String,String,String,i64,String,Option<String>,Option<String>,String)>("SELECT id, import_id, source_row_id, fd_player_id, fd_name, original_position, dfs_position, fd_team, salary, gsis_game_id, gsis_player_id, suggested_gsis_player_id, state FROM salary_import_rows WHERE import_id = ? ORDER BY (state = 'unmatched') DESC, id")
        .bind(import_id).fetch_all(pool).await?;
    rows.into_iter()
        .map(|r| {
            let position_value = position(r.6)?;
            Ok(SalaryImportRow {
                id: r.0,
                import_id: r.1,
                source_row_id: r.2,
                fd_player_id: r.3,
                fd_name: r.4,
                original_position: r.5,
                fd_team: r.7,
                dfs_position: position_value,
                salary: r.8,
                gsis_game_id: r.9,
                gsis_player_id: r.10,
                suggested_gsis_player_id: r.11,
                state: state(r.12)?,
            })
        })
        .collect()
}
pub(crate) async fn grouped_rows(
    pool: &SqlitePool,
    import_id: i64,
    group: RowGroup,
) -> Result<Vec<SalaryImportRow>, sqlx::Error> {
    let mut rows = rows_for(pool, import_id).await?;
    rows.retain(|row| row.state == group.state());
    Ok(rows)
}
pub(crate) async fn counts(pool: &SqlitePool, import_id: i64) -> Result<StateCounts, sqlx::Error> {
    let row = sqlx::query_as::<_, (i64,i64,i64,i64,i64)>("SELECT COALESCE(SUM(state='matched'),0), COALESCE(SUM(state='dst'),0), COALESCE(SUM(state='unmatched'),0), COALESCE(SUM(state='resolved'),0), COALESCE(SUM(state='skipped'),0) FROM salary_import_rows WHERE import_id = ?")
        .bind(import_id).fetch_one(pool).await?;
    Ok(StateCounts {
        matched: row.0,
        dst: row.1,
        unmatched: row.2,
        resolved: row.3,
        skipped: row.4,
    })
}

pub(super) async fn row_for(
    conn: &mut SqliteConnection,
    import_id: i64,
    row_id: i64,
) -> Result<Option<SalaryImportRow>, sqlx::Error> {
    // Read through the transaction connection so row membership and lifecycle are serialized.
    let found = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM salary_import_rows WHERE id = ? AND import_id = ?",
    )
    .bind(row_id)
    .bind(import_id)
    .fetch_one(&mut *conn)
    .await?;
    if found == 0 {
        return Ok(None);
    }
    let values = sqlx::query_as::<_, (i64,i64,i64,String,String,String,String,String,i64,String,Option<String>,Option<String>,String)>("SELECT id, import_id, source_row_id, fd_player_id, fd_name, original_position, dfs_position, fd_team, salary, gsis_game_id, gsis_player_id, suggested_gsis_player_id, state FROM salary_import_rows WHERE id = ? AND import_id = ?")
        .bind(row_id).bind(import_id).fetch_one(&mut *conn).await?;
    Ok(Some(SalaryImportRow {
        id: values.0,
        import_id: values.1,
        source_row_id: values.2,
        fd_player_id: values.3,
        fd_name: values.4,
        original_position: values.5,
        dfs_position: position(values.6)?,
        fd_team: values.7,
        salary: values.8,
        gsis_game_id: values.9,
        gsis_player_id: values.10,
        suggested_gsis_player_id: values.11,
        state: state(values.12)?,
    }))
}
pub(super) async fn require_pending(
    conn: &mut SqliteConnection,
    import_id: i64,
) -> Result<(), super::service::ImportError> {
    let status = sqlx::query_scalar::<_, String>("SELECT status FROM salary_imports WHERE id = ?")
        .bind(import_id)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or(super::service::ImportError::Missing)?;
    if status == "pending" {
        Ok(())
    } else {
        Err(super::service::ImportError::Conflict)
    }
}
