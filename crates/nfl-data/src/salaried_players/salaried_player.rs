use std::sync::Arc;
use nfl_model::{DfsPosition, Game, InjuryDesignation, NflPlayerId, TeamAbbr};

pub struct SalariedPlayer {
    pub id: Option<NflPlayerId>,
    pub game: Arc<Game>,
    pub name: String,
    pub position: DfsPosition,
    pub team: TeamAbbr,
    pub salary: u32,
    pub games_played: u8,
    pub fppg: f64,
    pub injury: Option<InjuryDesignation>
}
