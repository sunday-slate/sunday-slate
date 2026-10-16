use crate::Matchup;
use nfl_model::SeasonWeek;

/// The NFL season week and team matchup associated with salary entries.
#[derive(Debug, Clone, PartialEq)]
pub struct Game {
    /// NFL season and week, represented by a [`SeasonWeek`].
    pub season_week: SeasonWeek,
    /// Away and home teams for the game.
    pub matchup: Matchup,
}
