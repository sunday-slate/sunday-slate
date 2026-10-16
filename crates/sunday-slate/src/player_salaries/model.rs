pub use nfl_model::DfsPosition;

#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct NflPlayerSalary {
    pub id: i64,
    pub gsis_game_id: String,
    pub gsis_player_id: Option<String>,
    pub team_abbr: String,
    pub dfs_position: DfsPosition,
    pub salary: i64,
}
