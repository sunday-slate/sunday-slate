use nfl_model::{DfsPosition, TeamAbbr};
use crate::models::game::Game;
use crate::models::player::Player;

#[derive(Debug, Clone, PartialEq)]
pub struct SalariedPlayer {
    pub player: Option<Player>,
    pub game: Game,
    pub salary: u32,
    pub position: DfsPosition,
    pub team: TeamAbbr,
}   