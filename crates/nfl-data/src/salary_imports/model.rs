use nfl_model::DfsPosition;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RowState {
    Matched,
    Dst,
    Unmatched,
    Resolved,
    Skipped,
}
impl RowState {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Matched => "matched",
            Self::Dst => "dst",
            Self::Unmatched => "unmatched",
            Self::Resolved => "resolved",
            Self::Skipped => "skipped",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SalaryImport {
    pub id: i64,
    pub season: i64,
    pub week: i64,
    pub season_type: String,
    pub status: String,
    pub games: i64,
    pub source_upload_id: i64,
    pub received_at: String,
    pub committed_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SalaryImportRow {
    pub id: i64,
    pub import_id: i64,
    pub source_row_id: i64,
    pub fd_player_id: String,
    pub fd_name: String,
    pub fd_team: String,
    pub original_position: String,
    pub dfs_position: DfsPosition,
    pub salary: i64,
    pub gsis_game_id: String,
    pub gsis_player_id: Option<String>,
    pub suggested_gsis_player_id: Option<String>,
    pub state: RowState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RowGroup {
    Unmatched,
    Resolved,
    Skipped,
}
impl RowGroup {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Unmatched => "unmatched",
            Self::Resolved => "resolved",
            Self::Skipped => "skipped",
        }
    }

    pub(crate) fn state(self) -> RowState {
        match self {
            Self::Unmatched => RowState::Unmatched,
            Self::Resolved => RowState::Resolved,
            Self::Skipped => RowState::Skipped,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct StateCounts {
    pub matched: i64,
    pub dst: i64,
    pub unmatched: i64,
    pub resolved: i64,
    pub skipped: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlannedRow {
    pub source_row_id: i64,
    pub fd_player_id: String,
    pub fd_name: String,
    pub original_position: String,
    pub fd_team: String,
    pub dfs_position: DfsPosition,
    pub salary: i64,
    pub gsis_game_id: String,
    pub gsis_player_id: Option<String>,
    pub state: RowState,
    pub suggested_gsis_player_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SalaryPlan {
    pub season: i64,
    pub week: i64,
    pub season_type: String,
    pub games: usize,
    pub rows: Vec<PlannedRow>,
    pub diagnostics: Vec<String>,
}
