use super::{
    candidates,
    matcher::{PlayerIndex, RosterIndex},
    model::{PlannedRow, RowState, SalaryPlan},
    schedule::{ScheduleError, resolve_slate_games},
    store,
};
use crate::{NflData, SalaryError};
use nfl_model::{DfsPosition, Season, SeasonType, TeamAbbr, Week};
use std::collections::{BTreeMap, BTreeSet, HashMap};

#[derive(Debug, thiserror::Error)]
pub(crate) enum ImportError {
    #[error("salary import or row was not found")]
    Missing,
    #[error("salary import is not pending or the requested transition is not allowed")]
    Conflict,
    #[error("invalid salary import input: {0}")]
    InvalidInput(String),
    #[error("required NFL facts are unavailable: {0}")]
    Unavailable(String),
    #[error("salary import operation failed: {0}")]
    Internal(String),
}
impl From<sqlx::Error> for ImportError {
    fn from(e: sqlx::Error) -> Self {
        Self::Internal(e.to_string())
    }
}
impl From<SalaryError> for ImportError {
    fn from(e: SalaryError) -> Self {
        Self::Internal(e.to_string())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CommitReport {
    pub games: usize,
    pub salaries: usize,
}

pub(crate) struct SalaryImportService<'a> {
    nfl: &'a NflData,
}
impl<'a> SalaryImportService<'a> {
    pub(crate) fn new(nfl: &'a NflData) -> Self {
        Self { nfl }
    }

    pub(crate) async fn upload(
        &self,
        bytes: &[u8],
        filename: Option<&str>,
    ) -> Result<i64, ImportError> {
        let received = self
            .nfl
            .fanduel
            .receive(bytes, filename)
            .await
            .map_err(|e| ImportError::Internal(e.to_string()))?;
        match &received.interpretation {
            fanduel_data::Interpretation::Empty => {
                return Err(ImportError::InvalidInput("salary CSV is empty".into()));
            }
            fanduel_data::Interpretation::InvalidCsv(diagnostic) => {
                return Err(ImportError::InvalidInput(diagnostic.message.clone()));
            }
            fanduel_data::Interpretation::Parsed(_) => {}
        }
        let source_rows = self
            .nfl
            .fanduel
            .source_rows(received.upload_id)
            .await
            .map_err(|e| ImportError::Internal(e.to_string()))?;
        if source_rows.is_empty() {
            return Err(ImportError::InvalidInput(
                "salary CSV contains no salary rows".into(),
            ));
        }
        let matchups: BTreeSet<(String, String)> = source_rows
            .iter()
            .filter_map(|r| r.interpreted.matchup.as_ref())
            .map(|m| (m.away.0.clone(), m.home.0.clone()))
            .collect();
        if matchups.len() != source_rows.len()
            && source_rows.iter().any(|r| r.interpreted.matchup.is_none())
        {
            return Err(ImportError::InvalidInput(
                "every row must contain a valid matchup".into(),
            ));
        }
        if source_rows
            .iter()
            .any(|r| r.interpreted.position.is_none() || r.interpreted.salary.is_none())
        {
            return Err(ImportError::InvalidInput(
                "every row must have a supported position and numeric salary".into(),
            ));
        }
        let season = self.nfl.managed_season;
        let games = self
            .nfl
            .games(season)
            .await
            .map_err(|e| ImportError::Unavailable(e.to_string()))?;
        if games.is_empty() {
            return Err(ImportError::Unavailable(format!(
                "no cached schedule facts for season {}",
                season.0
            )));
        }
        let slate = resolve_slate_games(&games, &matchups).map_err(
            |ScheduleError::NoWeekContainsSlate| {
                ImportError::InvalidInput(
                    "could not resolve all uploaded matchups to one NFL week".into(),
                )
            },
        )?;
        let first = slate
            .first()
            .ok_or_else(|| ImportError::InvalidInput("no games were resolved".into()))?;
        if slate
            .iter()
            .any(|g| g.week != first.week || g.season_type != first.season_type)
        {
            return Err(ImportError::InvalidInput(
                "uploaded games span multiple schedule contexts".into(),
            ));
        }
        let mut game_by_team = HashMap::new();
        for game in &slate {
            game_by_team.insert(game.home_team.0.clone(), game.gsis_game_id.clone());
            game_by_team.insert(game.away_team.0.clone(), game.gsis_game_id.clone());
        }
        let players = self
            .nfl
            .players()
            .await
            .map_err(|e| ImportError::Unavailable(e.to_string()))?;
        let player_index = PlayerIndex::build(&players);
        let mut roster_entries = Vec::new();
        let teams: BTreeSet<String> = slate
            .iter()
            .flat_map(|g| [g.home_team.0.clone(), g.away_team.0.clone()])
            .collect();
        for team in teams {
            roster_entries.extend(
                self.nfl
                    .weekly_roster(season, first.week, &TeamAbbr(team))
                    .await
                    .map_err(|e| ImportError::Unavailable(e.to_string()))?,
            );
        }
        let roster_index = RosterIndex::build(&roster_entries);
        let mut plan = SalaryPlan {
            season: season.0 as i64,
            week: first.week.0 as i64,
            season_type: season_type_name(first.season_type),
            games: slate.len(),
            rows: Vec::new(),
            diagnostics: Vec::new(),
        };
        for source in source_rows {
            let row = &source.interpreted;
            let position = row.position.expect("validated above");
            let salary = row.salary.expect("validated above");
            let team = row.team.0.clone();
            let game = game_by_team.get(&team).cloned().ok_or_else(|| {
                ImportError::InvalidInput(format!(
                    "{} has no game in the resolved slate",
                    row.raw.name()
                ))
            })?;
            let fd_id = row.raw.fd_player_id().to_owned();
            let (gsis, state, suggestion) = if position == DfsPosition::Dst {
                (None, RowState::Dst, None)
            } else {
                let mapped = self.nfl.managed.mapping_by_fd_id(&fd_id).await?;
                let matched = mapped
                    .or_else(|| player_index.match_one(&row.raw.name(), &team))
                    .or_else(|| roster_index.match_one(&row.raw.name(), &team, position.label()));
                let suggestion = if matched.is_none() {
                    roster_index.suggest_one(&row.raw.name(), &team, position.label())
                } else {
                    None
                };
                let state = if matched.is_some() {
                    RowState::Matched
                } else {
                    RowState::Unmatched
                };
                (matched, state, suggestion)
            };
            plan.rows.push(PlannedRow {
                source_row_id: source.id,
                fd_player_id: fd_id,
                fd_name: row.raw.name(),
                original_position: row.raw.position.clone(),
                fd_team: team,
                dfs_position: position,
                salary,
                gsis_game_id: game,
                gsis_player_id: gsis,
                state,
                suggested_gsis_player_id: suggestion,
            });
            plan.diagnostics.extend(
                row.diagnostics
                    .iter()
                    .map(|d| format!("row {} {}: {}", d.row_number, d.field, d.message)),
            );
        }
        let upload_id = received.upload_id;
        self.nfl.managed.write_tx(async |conn| {
            let id: i64 = sqlx::query_scalar("INSERT INTO salary_imports(season, week, season_type, games, status, source_upload_id, received_at) VALUES (?, ?, ?, ?, 'pending', ?, datetime('now','subsec')) RETURNING id")
                .bind(plan.season).bind(plan.week).bind(&plan.season_type).bind(plan.games as i64).bind(upload_id).fetch_one(&mut *conn).await?;
            for row in &plan.rows {
                sqlx::query("INSERT INTO salary_import_rows(import_id, source_row_id, fd_player_id, fd_name, fd_team, original_position, dfs_position, salary, gsis_game_id, gsis_player_id, suggested_gsis_player_id, state) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)")
                    .bind(id).bind(row.source_row_id).bind(&row.fd_player_id).bind(&row.fd_name).bind(&row.fd_team).bind(row.original_position.to_uppercase()).bind(position_name(row.dfs_position)).bind(row.salary).bind(&row.gsis_game_id).bind(&row.gsis_player_id).bind(&row.suggested_gsis_player_id).bind(row.state.as_str()).execute(&mut *conn).await?;
            }
            Ok::<_, SalaryError>(id)
        }).await.map_err(ImportError::from)
    }

    pub(crate) async fn resolve(
        &self,
        import_id: i64,
        row_id: i64,
        player_id: &str,
    ) -> Result<(), ImportError> {
        let import = store::get(self.nfl.managed.reader(), import_id)
            .await?
            .ok_or(ImportError::Missing)?;
        if import.status != "pending" {
            return Err(ImportError::Conflict);
        }
        if player_id.trim().is_empty() {
            return Err(ImportError::InvalidInput("player id is required".into()));
        }
        let row = store::rows_for(self.nfl.managed.reader(), import_id)
            .await?
            .into_iter()
            .find(|r| r.id == row_id)
            .ok_or(ImportError::Missing)?;
        if !matches!(
            row.state,
            RowState::Unmatched | RowState::Resolved | RowState::Skipped
        ) {
            return Err(ImportError::Conflict);
        }
        let eligible = candidates::is_eligible(
            self.nfl,
            Season(import.season as u16),
            Week(import.week as u8),
            &row.fd_team,
            player_id,
        )
        .await
        .map_err(|e| ImportError::Unavailable(e.to_string()))?;
        if !eligible {
            return Err(ImportError::InvalidInput(
                "selected player is not an eligible canonical candidate".into(),
            ));
        }
        self.nfl.managed.write_tx(async |conn| {
            store::require_pending(conn, import_id).await?;
            let current = store::row_for(conn, import_id, row_id).await?.ok_or(ImportError::Missing)?;
            if !matches!(current.state, RowState::Unmatched | RowState::Resolved | RowState::Skipped) { return Err(ImportError::Conflict); }
            sqlx::query("UPDATE salary_import_rows SET gsis_player_id = ?, state = 'resolved' WHERE id = ? AND import_id = ?").bind(player_id).bind(row_id).bind(import_id).execute(&mut *conn).await?;
            Ok::<_, ImportError>(())
        }).await?;
        Ok(())
    }
    pub(crate) async fn skip(&self, import_id: i64, row_id: i64) -> Result<(), ImportError> {
        self.decide(import_id, row_id, None).await
    }
    pub(crate) async fn undo(&self, import_id: i64, row_id: i64) -> Result<(), ImportError> {
        self.nfl.managed.write_tx(async |conn| {
            store::require_pending(conn,import_id).await?;
            let row = store::row_for(conn,import_id,row_id).await?.ok_or(ImportError::Missing)?;
            if !matches!(row.state,RowState::Resolved|RowState::Skipped) { return Err(ImportError::Conflict); }
            sqlx::query("UPDATE salary_import_rows SET gsis_player_id=NULL,state='unmatched' WHERE id=? AND import_id=?").bind(row_id).bind(import_id).execute(&mut *conn).await?;
            Ok::<_,ImportError>(())
        }).await?;
        Ok(())
    }
    async fn decide(
        &self,
        import_id: i64,
        row_id: i64,
        player: Option<&str>,
    ) -> Result<(), ImportError> {
        self.nfl.managed.write_tx(async |conn| {
            store::require_pending(conn,import_id).await?;
            let row = store::row_for(conn,import_id,row_id).await?.ok_or(ImportError::Missing)?;
            if !matches!(row.state,RowState::Unmatched|RowState::Resolved|RowState::Skipped) { return Err(ImportError::Conflict); }
            sqlx::query("UPDATE salary_import_rows SET gsis_player_id=?,state=? WHERE id=? AND import_id=?").bind(player).bind(if player.is_some(){"resolved"}else{"skipped"}).bind(row_id).bind(import_id).execute(&mut *conn).await?;
            Ok::<_,ImportError>(())
        }).await?;
        Ok(())
    }
    pub(crate) async fn commit(&self, import_id: i64) -> Result<CommitReport, ImportError> {
        self.commit_with_hook(import_id, || async {}).await
    }

    #[cfg(test)]
    pub(crate) async fn commit_with_pre_transaction_hook<F, Fut>(
        &self,
        import_id: i64,
        before_transaction: F,
    ) -> Result<CommitReport, ImportError>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = ()>,
    {
        self.commit_with_hook(import_id, before_transaction).await
    }

    async fn commit_with_hook<F, Fut>(
        &self,
        import_id: i64,
        before_transaction: F,
    ) -> Result<CommitReport, ImportError>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = ()>,
    {
        let preview = store::get(self.nfl.managed.reader(), import_id)
            .await?
            .ok_or(ImportError::Missing)?;
        if preview.status != "pending" {
            return Err(ImportError::Conflict);
        }
        let validated_rows = store::rows_for(self.nfl.managed.reader(), import_id).await?;
        let validated_snapshot: BTreeMap<i64, (String, Option<String>)> = validated_rows
            .iter()
            .map(|row| {
                (
                    row.id,
                    (row.state.as_str().to_owned(), row.gsis_player_id.clone()),
                )
            })
            .collect();
        for row in validated_rows {
            if matches!(row.state, RowState::Resolved | RowState::Matched) {
                let Some(player_id) = row.gsis_player_id.as_deref() else {
                    return Err(ImportError::Conflict);
                };
                let valid = if row.state == RowState::Matched {
                    self.matched_identity_exists(&row.fd_player_id, player_id)
                        .await?
                } else {
                    candidates::is_eligible(
                        self.nfl,
                        Season(preview.season as u16),
                        Week(preview.week as u8),
                        &row.fd_team,
                        player_id,
                    )
                    .await
                    .map_err(|e| ImportError::Unavailable(e.to_string()))?
                };
                if !valid {
                    return Err(ImportError::InvalidInput(
                        "selected player is no longer an eligible canonical candidate".into(),
                    ));
                }
            }
        }
        before_transaction().await;
        self.nfl.managed.write_tx(async |conn| {
            let batch = sqlx::query_as::<_,(i64,i64,i64,String,String)>("SELECT season,week,games,season_type,status FROM salary_imports WHERE id=?").bind(import_id).fetch_optional(&mut *conn).await?.ok_or(ImportError::Missing)?;
            if batch.4 != "pending" { return Err(ImportError::Conflict); }
            let rows = sqlx::query_as::<_,(i64,String,String,String,i64,String,Option<String>,String)>("SELECT id,fd_player_id,fd_team,dfs_position,salary,gsis_game_id,gsis_player_id,state FROM salary_import_rows WHERE import_id=?").bind(import_id).fetch_all(&mut *conn).await?;
            let transaction_snapshot: BTreeMap<i64, (String, Option<String>)> = rows
                .iter()
                .map(|row| (row.0, (row.7.clone(), row.6.clone())))
                .collect();
            if transaction_snapshot != validated_snapshot {
                return Err(ImportError::Conflict);
            }
            if rows.iter().any(|r| !matches!(r.7.as_str(), "matched" | "dst" | "unmatched" | "resolved" | "skipped") || r.7 == "unmatched" || ((r.7 == "resolved" || r.7 == "matched") && r.6.is_none()) || (r.7 == "dst" && r.6.is_some())) { return Err(ImportError::Conflict); }
            let mut subjects = std::collections::HashSet::new();
            let mut provider_mappings = HashMap::new();
            for row in rows.iter().filter(|r| r.7 != "skipped") {
                let key = if row.3 == "DST" { format!("{}:DST:{}", row.5, row.2) } else { format!("{}:PLAYER:{}", row.5, row.6.as_deref().unwrap_or_default()) };
                if !subjects.insert(key) { return Err(ImportError::Conflict); }
                if matches!(row.7.as_str(), "resolved" | "matched") {
                    let gsis = row.6.as_deref().ok_or(ImportError::Conflict)?;
                    if provider_mappings.insert(row.1.as_str(), gsis).is_some_and(|prior| prior != gsis) {
                        return Err(ImportError::Conflict);
                    }
                }
            }
            let mut salaries=0;
            for row in rows.iter().filter(|r| r.7 != "skipped") {
                if matches!(row.7.as_str(), "resolved" | "matched") {
                    let gsis = row.6.as_deref().ok_or(ImportError::Conflict)?;
                    let by_provider: Option<String> = sqlx::query_scalar("SELECT gsis_player_id FROM confirmed_player_mappings WHERE fd_player_id=?").bind(&row.1).fetch_optional(&mut *conn).await?;
                    if by_provider.as_deref().is_some_and(|existing| existing != gsis) { return Err(ImportError::Conflict); }
                    let by_canonical: Option<String> = sqlx::query_scalar("SELECT fd_player_id FROM confirmed_player_mappings WHERE gsis_player_id=?").bind(gsis).fetch_optional(&mut *conn).await?;
                    if by_canonical.as_deref().is_some_and(|existing| existing != row.1) { return Err(ImportError::Conflict); }
                    sqlx::query("INSERT INTO confirmed_player_mappings(fd_player_id,gsis_player_id,created_at,updated_at) VALUES(?,?,datetime('now','subsec'),datetime('now','subsec')) ON CONFLICT(fd_player_id) DO UPDATE SET gsis_player_id=excluded.gsis_player_id,updated_at=excluded.updated_at WHERE confirmed_player_mappings.gsis_player_id <> excluded.gsis_player_id").bind(&row.1).bind(gsis).execute(&mut *conn).await?;
                }
                let position = if row.3 == "D" || row.3 == "DEF" { "DST" } else { row.3.as_str() };
                let subject = row.6.as_deref();
                sqlx::query("INSERT INTO current_salaries(gsis_game_id,gsis_player_id,team_abbr,dfs_position,salary,provenance,import_id,source_row_id,created_at,updated_at) VALUES(?,?,?,?,?,'import',?,?,datetime('now','subsec'),datetime('now','subsec')) ON CONFLICT DO UPDATE SET team_abbr=excluded.team_abbr,dfs_position=excluded.dfs_position,salary=excluded.salary,provenance='import',import_id=excluded.import_id,source_row_id=excluded.source_row_id,updated_at=excluded.updated_at")
                    .bind(&row.5).bind(subject).bind(&row.2).bind(position).bind(row.4).bind(import_id).bind(row.0).execute(&mut *conn).await?;
                salaries += 1;
            }
            let now = sqlx::query("UPDATE salary_imports SET status='committed',committed_at=datetime('now','subsec') WHERE id=? AND status='pending'").bind(import_id).execute(&mut *conn).await?;
            if now.rows_affected()!=1 { return Err(ImportError::Conflict); }
            Ok::<_,ImportError>(CommitReport { games:batch.2 as usize,salaries })
        }).await
    }
    async fn matched_identity_exists(
        &self,
        fd_player_id: &str,
        gsis_player_id: &str,
    ) -> Result<bool, ImportError> {
        let ids = [gsis_player_id];
        let players = self
            .nfl
            .players_by_gsis(&ids)
            .await
            .map_err(|e| ImportError::Unavailable(e.to_string()))?;
        if players.contains_key(gsis_player_id) {
            return Ok(true);
        }
        let roster = self
            .nfl
            .weekly_roster_entries_by_gsis(&ids)
            .await
            .map_err(|e| ImportError::Unavailable(e.to_string()))?;
        if roster.contains_key(gsis_player_id) {
            return Ok(true);
        }
        Ok(self
            .nfl
            .managed
            .mapping_by_fd_id(fd_player_id)
            .await?
            .as_deref()
            == Some(gsis_player_id))
    }

    pub(crate) async fn discard(&self, import_id: i64) -> Result<(), ImportError> {
        self.nfl
            .managed
            .write_tx(async |conn| {
                store::require_pending(conn, import_id).await?;
                let result =
                    sqlx::query("DELETE FROM salary_imports WHERE id=? AND status='pending'")
                        .bind(import_id)
                        .execute(&mut *conn)
                        .await?;
                if result.rows_affected() != 1 {
                    return Err(ImportError::Missing);
                }
                Ok::<_, ImportError>(())
            })
            .await?;
        Ok(())
    }
}
fn position_name(p: DfsPosition) -> &'static str {
    match p {
        DfsPosition::Qb => "QB",
        DfsPosition::Rb => "RB",
        DfsPosition::Wr => "WR",
        DfsPosition::Te => "TE",
        DfsPosition::Dst => "DST",
    }
}
fn season_type_name(t: SeasonType) -> String {
    match t {
        SeasonType::Reg => "REG",
        SeasonType::Post => "POST",
    }
    .into()
}
