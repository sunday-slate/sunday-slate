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
    /// The latest Tank01 report status for an eligible slate. This is current
    /// input, not the player's historical game status or an availability
    /// guarantee; `None` means no matching report entry.
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
