use crate::models::game::Game;
use crate::models::player::Player;
use nfl_model::{DfsPosition, TeamAbbr};

/// A player's or defense's FanDuel salary entry for a game.
///
/// `player` is `None` for defense/special teams (`DST`) entries and `Some` for all other
/// positions.
#[derive(Debug, Clone, PartialEq)]
pub struct SalariedPlayer {
    /// Player associated with the entry, or `None` for `DST`.
    pub player: Option<Player>,
    /// Game and season week associated with the entry.
    pub game: Game,
    /// Salary in FanDuel's export units (typically whole dollars).
    pub salary: u32,
    /// FanDuel DFS roster position; see [`DfsPosition`] for supported positions.
    pub position: DfsPosition,
    /// Team represented by this entry, as a [`TeamAbbr`].
    pub team: TeamAbbr,
}
