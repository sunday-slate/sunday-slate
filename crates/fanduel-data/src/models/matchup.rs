use crate::FanduelDataError;
use nfl_model::TeamAbbr;
use std::fmt::{Display, Formatter};
use std::str::FromStr;

/// The away and home teams in a game.
///
/// Parses from and displays as `AWAY@HOME` (for example, `BUF@NYJ`), using [`TeamAbbr`]
/// for each team.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Matchup {
    /// Away team abbreviation.
    pub away: TeamAbbr,
    /// Home team abbreviation.
    pub home: TeamAbbr,
}

impl FromStr for Matchup {
    type Err = FanduelDataError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (away, home) = s
            .split_once('@')
            .ok_or(FanduelDataError::MatchupParseError(s.to_string()))?;
        let home = home
            .parse::<TeamAbbr>()
            .map_err(|_| FanduelDataError::MatchupParseError(s.to_owned()))?;
        let away = away
            .parse::<TeamAbbr>()
            .map_err(|_| FanduelDataError::MatchupParseError(s.to_owned()))?;
        Ok(Matchup { away, home })
    }
}

impl Display for Matchup {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}@{}", self.away, self.home)
    }
}
