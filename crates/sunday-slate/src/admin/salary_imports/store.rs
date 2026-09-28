use sqlx::Sqlite;

use crate::Db;
use crate::admin::salary_imports::SalaryPlan;
use crate::admin::salary_imports::model::{RowGroup, RowState, SalaryImport, SalaryImportRow};

/// Insert a batch and all its planned rows in one transaction; returns the batch id.
pub async fn stage(
    db: &Db,
    plan: &SalaryPlan,
    created_by: Option<i64>,
) -> Result<i64, sqlx::Error> {
    let season = plan.season as i64;
    let week = plan.week.map(i64::from);
    let games = plan.games as i64;
    db.write_tx::<_, i64, sqlx::Error>(async |conn| {
        let id: i64 = sqlx::query_scalar!(
            r#"INSERT INTO salary_imports (season, week, games, created_by)
               VALUES (?1, ?2, ?3, ?4) RETURNING id AS "id!: i64""#,
            season,
            week,
            games,
            created_by,
        )
        .fetch_one(&mut *conn)
        .await?;

        for r in &plan.rows {
            let pos = r.dfs_position;
            let state = r.state;
            sqlx::query!(
                r#"INSERT INTO salary_import_rows
                     (import_id, fd_player_id, fd_name, fd_team, dfs_position,
                      salary, gsis_game_id, gsis_player_id, state,
                      suggested_gsis_player_id)
                   VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)"#,
                id,
                r.fd_player_id,
                r.fd_name,
                r.fd_team,
                pos,
                r.salary,
                r.gsis_game_id,
                r.gsis_player_id,
                state,
                r.suggested_gsis_player_id,
            )
            .execute(&mut *conn)
            .await?;
        }
        Ok(id)
    })
    .await
}

/// One batch by id.
pub async fn get<'e, E>(ex: E, id: i64) -> Result<Option<SalaryImport>, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    sqlx::query_as!(
        SalaryImport,
        r#"SELECT id AS "id!: i64", season AS "season!: i64",
                  week AS "week: i64", status,
                  games AS "games!: i64", created_by AS "created_by: i64",
                  created_at, committed_at AS "committed_at: String"
           FROM salary_imports WHERE id = ?"#,
        id,
    )
    .fetch_optional(ex)
    .await
}

/// All batches, newest first (index page).
pub async fn list_recent<'e, E>(ex: E, limit: i64) -> Result<Vec<SalaryImport>, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    sqlx::query_as!(
        SalaryImport,
        r#"SELECT id AS "id!: i64", season AS "season!: i64",
                  week AS "week: i64", status,
                  games AS "games!: i64", created_by AS "created_by: i64",
                  created_at, committed_at AS "committed_at: String"
           FROM salary_imports ORDER BY id DESC LIMIT ?"#,
        limit,
    )
    .fetch_all(ex)
    .await
}

/// All rows of a batch, stable order (unmatched first, then by id).
pub async fn rows_for<'e, E>(ex: E, import_id: i64) -> Result<Vec<SalaryImportRow>, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    sqlx::query_as!(
        SalaryImportRow,
        r#"SELECT id AS "id!: i64", import_id AS "import_id!: i64",
                  fd_player_id, fd_name, fd_team,
                  dfs_position AS "dfs_position!: crate::player_salaries::model::DfsPosition",
                  salary AS "salary!: i64", gsis_game_id,
                  gsis_player_id AS "gsis_player_id: String",
                  suggested_gsis_player_id AS "suggested_gsis_player_id: String",
                  state AS "state!: RowState"
           FROM salary_import_rows WHERE import_id = ?
           ORDER BY (state = 'unmatched') DESC, id"#,
        import_id,
    )
    .fetch_all(ex)
    .await
}

/// Count rows still needing a decision.
pub async fn unmatched_count<'e, E>(ex: E, import_id: i64) -> Result<i64, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    sqlx::query_scalar!(
        r#"SELECT COUNT(*) FROM salary_import_rows
           WHERE import_id = ? AND state = 'unmatched'"#,
        import_id,
    )
    .fetch_one(ex)
    .await
}

/// Apply one decision: `Some(gsis)` → resolved; `None` → skipped. Scoped to the
/// rows a decision can be made about, so a hand-crafted POST naming an
/// auto-matched row of the same batch can neither skip it out of the commit nor
/// rewrite the match the importer made. A row the admin has already decided may
/// be decided again.
pub async fn resolve_row(
    conn: &mut sqlx::SqliteConnection,
    import_id: i64,
    row_id: i64,
    gsis_player_id: Option<&str>,
) -> Result<(), sqlx::Error> {
    let state = if gsis_player_id.is_some() {
        RowState::Resolved
    } else {
        RowState::Skipped
    };
    sqlx::query!(
        r#"UPDATE salary_import_rows
           SET gsis_player_id = ?1, state = ?2
           WHERE id = ?3 AND import_id = ?4
             AND state IN ('unmatched', 'resolved', 'skipped')"#,
        gsis_player_id,
        state,
        row_id,
        import_id,
    )
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Delete a batch (rows cascade).
pub async fn delete(conn: &mut sqlx::SqliteConnection, id: i64) -> Result<(), sqlx::Error> {
    sqlx::query!("DELETE FROM salary_imports WHERE id = ?", id)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// Row counts by state for one batch.
pub struct StateCounts {
    pub matched: i64,
    pub dst: i64,
    pub unmatched: i64,
    pub resolved: i64,
    pub skipped: i64,
}

impl StateCounts {
    /// Rows the importer settled on its own — never rendered.
    pub fn auto_matched(&self) -> i64 {
        self.matched + self.dst
    }

    pub fn count_for(&self, group: RowGroup) -> i64 {
        match group {
            RowGroup::Unmatched => self.unmatched,
            RowGroup::Resolved => self.resolved,
            RowGroup::Skipped => self.skipped,
        }
    }
}

/// Rows of one actionable group, in upload order.
pub async fn decision_rows_for<'e, E>(
    ex: E,
    import_id: i64,
    group: RowGroup,
) -> Result<Vec<SalaryImportRow>, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    let state = group.state();
    sqlx::query_as!(
        SalaryImportRow,
        r#"SELECT id AS "id!: i64", import_id AS "import_id!: i64",
                  fd_player_id, fd_name, fd_team,
                  dfs_position AS "dfs_position!: crate::player_salaries::model::DfsPosition",
                  salary AS "salary!: i64", gsis_game_id,
                  gsis_player_id AS "gsis_player_id: String",
                  suggested_gsis_player_id AS "suggested_gsis_player_id: String",
                  state AS "state!: RowState"
           FROM salary_import_rows
           WHERE import_id = ? AND state = ?
           ORDER BY id"#,
        import_id,
        state,
    )
    .fetch_all(ex)
    .await
}

/// Every state's count in one pass.
pub async fn state_counts<'e, E>(ex: E, import_id: i64) -> Result<StateCounts, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    let row = sqlx::query!(
        r#"SELECT
             COALESCE(SUM(state = 'matched'), 0)   AS "matched!: i64",
             COALESCE(SUM(state = 'dst'), 0)       AS "dst!: i64",
             COALESCE(SUM(state = 'unmatched'), 0) AS "unmatched!: i64",
             COALESCE(SUM(state = 'resolved'), 0)  AS "resolved!: i64",
             COALESCE(SUM(state = 'skipped'), 0)   AS "skipped!: i64"
           FROM salary_import_rows WHERE import_id = ?"#,
        import_id,
    )
    .fetch_one(ex)
    .await?;
    Ok(StateCounts {
        matched: row.matched,
        dst: row.dst,
        unmatched: row.unmatched,
        resolved: row.resolved,
        skipped: row.skipped,
    })
}

/// Undo a decision: back to `unmatched` with no pick. Scoped to the two states
/// an admin can produce, so the route can never un-match an auto-matched row.
pub async fn unresolve_row(
    conn: &mut sqlx::SqliteConnection,
    import_id: i64,
    row_id: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        r#"UPDATE salary_import_rows
           SET gsis_player_id = NULL, state = 'unmatched'
           WHERE id = ?1 AND import_id = ?2 AND state IN ('resolved', 'skipped')"#,
        row_id,
        import_id,
    )
    .execute(&mut *conn)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::admin::salary_imports::model::{RowGroup, RowState};
    use crate::admin::salary_imports::{PlannedRow, SalaryPlan};
    use crate::player_salaries::model::DfsPosition;
    use sqlx::SqlitePool;

    fn plan_with(rows: Vec<PlannedRow>) -> SalaryPlan {
        SalaryPlan {
            season: 2025,
            week: Some(1),
            games: 1,
            rows,
            offending: vec![],
        }
    }

    fn matched(fd: &str, gsis: &str) -> PlannedRow {
        PlannedRow {
            fd_player_id: fd.into(),
            fd_name: "Josh Allen".into(),
            fd_team: "BUF".into(),
            dfs_position: DfsPosition::Qb,
            salary: 8000,
            gsis_game_id: "2025_01_BUF_NYJ".into(),
            gsis_player_id: Some(gsis.into()),
            state: RowState::Matched,
            suggested_gsis_player_id: None,
        }
    }

    fn unmatched(fd: &str) -> PlannedRow {
        PlannedRow {
            fd_player_id: fd.into(),
            fd_name: "Nobody Here".into(),
            fd_team: "BUF".into(),
            dfs_position: DfsPosition::Wr,
            salary: 5000,
            gsis_game_id: "2025_01_BUF_NYJ".into(),
            gsis_player_id: None,
            state: RowState::Unmatched,
            suggested_gsis_player_id: None,
        }
    }

    #[sqlx::test]
    async fn stage_then_read_back(pool: SqlitePool) {
        let db = crate::Db::test(pool.clone());
        // salary_imports.created_by references users(id); seed the row it points at.
        sqlx::query("INSERT INTO users (id, email) VALUES (7, 'admin@test.local')")
            .execute(&pool)
            .await
            .expect("seed user");
        let id = stage(
            &db,
            &plan_with(vec![matched("100", "00-1"), unmatched("200")]),
            Some(7),
        )
        .await
        .expect("stage");

        let batch = get(&pool, id).await.expect("get").expect("some");
        assert_eq!(batch.season, 2025);
        assert_eq!(batch.status, "pending");
        assert_eq!(batch.created_by, Some(7));

        let rows = rows_for(&pool, id).await.expect("rows");
        assert_eq!(rows.len(), 2);
        assert_eq!(unmatched_count(&pool, id).await.expect("count"), 1);
    }

    #[sqlx::test]
    async fn resolve_flips_state_and_sets_gsis(pool: SqlitePool) {
        let db = crate::Db::test(pool.clone());
        let id = stage(&db, &plan_with(vec![unmatched("200")]), None)
            .await
            .expect("stage");
        let row_id = rows_for(&pool, id).await.unwrap()[0].id;

        db.write_tx::<_, (), sqlx::Error>(async |conn| {
            resolve_row(&mut *conn, id, row_id, Some("00-7")).await
        })
        .await
        .expect("resolve");

        let row = rows_for(&pool, id).await.unwrap().remove(0);
        assert_eq!(row.state, RowState::Resolved);
        assert_eq!(row.gsis_player_id.as_deref(), Some("00-7"));
        assert_eq!(unmatched_count(&pool, id).await.unwrap(), 0);
    }

    #[sqlx::test]
    async fn skip_flips_state_to_skipped(pool: SqlitePool) {
        let db = crate::Db::test(pool.clone());
        let id = stage(&db, &plan_with(vec![unmatched("200")]), None)
            .await
            .unwrap();
        let row_id = rows_for(&pool, id).await.unwrap()[0].id;

        db.write_tx::<_, (), sqlx::Error>(async |conn| {
            resolve_row(&mut *conn, id, row_id, None).await
        })
        .await
        .unwrap();

        assert_eq!(
            rows_for(&pool, id).await.unwrap()[0].state,
            RowState::Skipped
        );
        assert_eq!(unmatched_count(&pool, id).await.unwrap(), 0);
    }

    fn dst(fd: &str) -> PlannedRow {
        PlannedRow {
            fd_player_id: fd.into(),
            fd_name: "Buffalo Bills".into(),
            fd_team: "BUF".into(),
            dfs_position: DfsPosition::Dst,
            salary: 3000,
            gsis_game_id: "2025_01_BUF_NYJ".into(),
            gsis_player_id: None,
            state: RowState::Dst,
            suggested_gsis_player_id: None,
        }
    }

    /// A batch holding one row in every state: matched, dst, resolved, skipped,
    /// and two still unmatched (two, so ordering assertions have something to
    /// compare). Returns the batch id.
    async fn staged_all_states(pool: &SqlitePool) -> i64 {
        let db = crate::Db::test(pool.clone());
        sqlx::query("INSERT INTO users (id, email) VALUES (7, 'admin@test.local')")
            .execute(pool)
            .await
            .expect("seed user");
        let id = stage(
            &db,
            &plan_with(vec![
                matched("100", "00-1"),
                dst("101"),
                unmatched("200"),
                unmatched("201"),
                unmatched("202"),
                unmatched("203"),
            ]),
            Some(7),
        )
        .await
        .expect("stage");

        let ids: Vec<i64> = rows_for(pool, id)
            .await
            .expect("rows")
            .into_iter()
            .filter(|r| r.state == RowState::Unmatched)
            .map(|r| r.id)
            .collect();
        let mut conn = pool.acquire().await.expect("conn");
        resolve_row(&mut conn, id, ids[0], Some("00-9"))
            .await
            .expect("resolve");
        resolve_row(&mut conn, id, ids[1], None)
            .await
            .expect("skip");
        id
    }

    #[sqlx::test]
    async fn decision_rows_are_scoped_to_one_group_and_ordered_by_id(pool: SqlitePool) {
        let import_id = staged_all_states(&pool).await;

        let rows = decision_rows_for(&pool, import_id, RowGroup::Unmatched)
            .await
            .expect("unmatched group");
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| r.state == RowState::Unmatched));
        let ids: Vec<i64> = rows.iter().map(|r| r.id).collect();
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        assert_eq!(ids, sorted, "ordered by id");

        assert_eq!(
            decision_rows_for(&pool, import_id, RowGroup::Resolved)
                .await
                .expect("resolved group")
                .len(),
            1
        );
        assert_eq!(
            decision_rows_for(&pool, import_id, RowGroup::Skipped)
                .await
                .expect("skipped group")
                .len(),
            1
        );

        // Auto-matched rows belong to no group.
        for group in RowGroup::ALL {
            let rows = decision_rows_for(&pool, import_id, group)
                .await
                .expect("group");
            assert!(
                !rows
                    .iter()
                    .any(|r| matches!(r.state, RowState::Matched | RowState::Dst)),
                "{group:?} must not contain auto-matched rows"
            );
        }
    }

    #[sqlx::test]
    async fn state_counts_counts_every_state(pool: SqlitePool) {
        let import_id = staged_all_states(&pool).await;
        let counts = state_counts(&pool, import_id).await.expect("counts");

        assert_eq!(counts.matched, 1);
        assert_eq!(counts.dst, 1);
        assert_eq!(counts.unmatched, 2);
        assert_eq!(counts.resolved, 1);
        assert_eq!(counts.skipped, 1);
        assert_eq!(counts.auto_matched(), 2);
        assert_eq!(counts.count_for(RowGroup::Unmatched), 2);
        assert_eq!(counts.count_for(RowGroup::Resolved), 1);
        assert_eq!(counts.count_for(RowGroup::Skipped), 1);
        assert_eq!(
            counts.matched + counts.dst + counts.unmatched + counts.resolved + counts.skipped,
            rows_for(&pool, import_id).await.expect("rows").len() as i64
        );
    }

    /// The undo route takes a row id. A hand-crafted request must not be able to
    /// un-match a row the importer matched automatically.
    #[sqlx::test]
    async fn unresolve_refuses_to_touch_an_auto_matched_row(pool: SqlitePool) {
        let import_id = staged_all_states(&pool).await;
        let matched_id = rows_for(&pool, import_id)
            .await
            .expect("rows")
            .into_iter()
            .find(|r| r.state == RowState::Matched)
            .expect("a matched row")
            .id;

        let mut conn = pool.acquire().await.expect("conn");
        unresolve_row(&mut conn, import_id, matched_id)
            .await
            .expect("unresolve");

        let still = rows_for(&pool, import_id)
            .await
            .expect("rows")
            .into_iter()
            .find(|r| r.id == matched_id)
            .expect("row");
        assert_eq!(still.state, RowState::Matched, "matched row untouched");
        assert!(still.gsis_player_id.is_some(), "its match survived");
    }

    /// The resolve route takes a row id too. A hand-crafted POST naming an
    /// auto-matched row of the same batch must not be able to skip it — which
    /// would silently drop it from the commit — or rewrite the match the
    /// importer made.
    #[sqlx::test]
    async fn resolve_refuses_to_touch_an_auto_matched_row(pool: SqlitePool) {
        let import_id = staged_all_states(&pool).await;
        let auto: Vec<i64> = rows_for(&pool, import_id)
            .await
            .expect("rows")
            .into_iter()
            .filter(|r| matches!(r.state, RowState::Matched | RowState::Dst))
            .map(|r| r.id)
            .collect();
        assert_eq!(auto.len(), 2, "one matched, one D/ST");

        let mut conn = pool.acquire().await.expect("conn");
        for id in &auto {
            // Both shapes of decision: a skip, and a pick that would rewrite the
            // importer's own match.
            resolve_row(&mut conn, import_id, *id, None)
                .await
                .expect("skip");
            resolve_row(&mut conn, import_id, *id, Some("00-hijack"))
                .await
                .expect("resolve");
        }

        let rows = rows_for(&pool, import_id).await.expect("rows");
        let matched = rows
            .iter()
            .find(|r| r.id == auto[0])
            .expect("the matched row");
        assert_eq!(matched.state, RowState::Matched, "matched row untouched");
        assert_eq!(
            matched.gsis_player_id.as_deref(),
            Some("00-1"),
            "its match survived"
        );
        let dst = rows.iter().find(|r| r.id == auto[1]).expect("the D/ST row");
        assert_eq!(dst.state, RowState::Dst, "D/ST row untouched");
        assert!(dst.gsis_player_id.is_none(), "and gained no pick");
    }

    /// A row the admin has already decided may be decided again — changing your
    /// mind is the point of the resolved and skipped tabs.
    #[sqlx::test]
    async fn resolve_can_re_decide_a_decided_row(pool: SqlitePool) {
        let import_id = staged_all_states(&pool).await;
        let skipped = rows_for(&pool, import_id)
            .await
            .expect("rows")
            .into_iter()
            .find(|r| r.state == RowState::Skipped)
            .expect("a skipped row")
            .id;

        let mut conn = pool.acquire().await.expect("conn");
        resolve_row(&mut conn, import_id, skipped, Some("00-9"))
            .await
            .expect("re-decide");

        let row = rows_for(&pool, import_id)
            .await
            .expect("rows")
            .into_iter()
            .find(|r| r.id == skipped)
            .expect("row");
        assert_eq!(row.state, RowState::Resolved);
        assert_eq!(row.gsis_player_id.as_deref(), Some("00-9"));
    }

    #[sqlx::test]
    async fn unresolve_returns_decided_rows_to_unmatched(pool: SqlitePool) {
        let import_id = staged_all_states(&pool).await;
        let decided: Vec<i64> = rows_for(&pool, import_id)
            .await
            .expect("rows")
            .into_iter()
            .filter(|r| matches!(r.state, RowState::Resolved | RowState::Skipped))
            .map(|r| r.id)
            .collect();
        assert_eq!(decided.len(), 2, "one resolved, one skipped");

        let mut conn = pool.acquire().await.expect("conn");
        for id in &decided {
            unresolve_row(&mut conn, import_id, *id)
                .await
                .expect("unresolve");
        }

        let rows = rows_for(&pool, import_id).await.expect("rows");
        for id in &decided {
            let back = rows.iter().find(|r| r.id == *id).expect("row");
            assert_eq!(back.state, RowState::Unmatched);
            assert!(back.gsis_player_id.is_none(), "pick cleared");
        }
        assert_eq!(unmatched_count(&pool, import_id).await.expect("count"), 4);
    }
}
