use crate::scoring::Score;
use nfl_model::{DfsPosition, Game, NflPlayerId, PlayerWeekStats, TeamAbbr, TeamWeekStats};
use std::sync::Arc;

#[derive(Clone)]
pub struct SalariedPlayer {
    pub id: Option<NflPlayerId>,
    pub game: Arc<Game>,
    pub name: String,
    pub position: DfsPosition,
    pub team: TeamAbbr,
    pub salary: u32,
    pub games_played: u8,
    pub fppg: f64,
    /// This player's stats and score for its slate's NFL week, when the week's
    /// stats include that player or team.
    pub scored_stats: Option<ScoredStats>,
}

/// The week stat record an attachment was scored from.
#[derive(Clone, Debug, PartialEq)]
pub enum WeeklyStats {
    Player(PlayerWeekStats),
    Defense(TeamWeekStats),
}

/// A player's stats for its slate's week and the score computed from them.
#[derive(Clone, Debug, PartialEq)]
pub struct ScoredStats {
    pub stats: WeeklyStats,
    pub score: Score,
}
