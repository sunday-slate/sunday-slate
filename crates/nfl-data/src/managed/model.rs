use nfl_model::{DfsPosition, TeamAbbr};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Salary {
    pub gsis_game_id: String,
    pub gsis_player_id: Option<String>,
    pub team_abbr: TeamAbbr,
    pub dfs_position: DfsPosition,
    pub salary: i64,
}

#[derive(Debug, Clone)]
pub(super) struct LegacySalary {
    pub game_id: String,
    pub player_id: Option<String>,
    pub team: String,
    pub position: String,
    pub salary: i64,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone)]
pub(super) struct LegacyMapping {
    pub fd_player_id: String,
    pub gsis_player_id: String,
    pub created_at: String,
    pub updated_at: String,
}
