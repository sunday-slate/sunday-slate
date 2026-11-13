use crate::scoring::Score;
use nfl_model::{
    DfsPosition, Game, InjuryDesignation, NflPlayerId, PlayerWeekStats, TeamAbbr, TeamWeekStats,
};
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct SalariedPlayer {
    pub id: Option<NflPlayerId>,
    pub game: Arc<Game>,
    pub name: String,
    pub position: DfsPosition,
    pub team: TeamAbbr,
    pub salary: u32,
    pub games_played: u8,
    pub fppg: f64,
    /// Latest report status on the current slate, not historical game status.
    pub injury: Option<InjuryDesignation>,
    pub scored_stats: Option<ScoredStats>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum WeeklyStats {
    Player(PlayerWeekStats),
    Defense(TeamWeekStats),
}

#[derive(Clone, Debug, PartialEq)]
pub struct ScoredStats {
    pub stats: WeeklyStats,
    pub score: Score,
}
