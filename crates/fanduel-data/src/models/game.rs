use crate::Matchup;
use nfl_model::SeasonWeek;

#[derive(Debug, Clone, PartialEq)]
pub struct Game {
    pub season_week: SeasonWeek,
    pub matchup: Matchup,
}
