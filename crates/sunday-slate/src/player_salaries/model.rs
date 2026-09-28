#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type)]
#[sqlx(rename_all = "UPPERCASE")]
pub enum DfsPosition {
    Qb,
    Rb,
    Wr,
    Te,
    Dst,
}

impl DfsPosition {
    pub fn is_defense(self) -> bool {
        matches!(self, DfsPosition::Dst)
    }

    /// How FanDuel writes the position, for display next to a staged row.
    pub fn label(self) -> &'static str {
        match self {
            DfsPosition::Qb => "QB",
            DfsPosition::Rb => "RB",
            DfsPosition::Wr => "WR",
            DfsPosition::Te => "TE",
            DfsPosition::Dst => "D/ST",
        }
    }
}

#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct NflPlayerSalary {
    pub id: i64,
    pub gsis_game_id: String,
    pub gsis_player_id: Option<String>,
    pub team_abbr: String,
    pub dfs_position: DfsPosition,
    pub salary: i64,
}
