use nfl_model::SeasonWeek;
use crate::Matchup;

#[derive(Debug, Clone, PartialEq)]
pub struct Game {
    pub season_week: SeasonWeek,
    pub matchup: Matchup
}
