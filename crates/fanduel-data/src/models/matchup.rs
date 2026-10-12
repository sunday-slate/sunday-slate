use std::fmt::{Display, Formatter};
use std::str::FromStr;
use nfl_model::TeamAbbr;
use crate::FanduelDataError;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Matchup {
    pub away: TeamAbbr,
    pub home: TeamAbbr,
}

impl FromStr for Matchup {
    type Err = FanduelDataError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (away, home) = s.split_once('@').ok_or(FanduelDataError::MatchupParseError(s.to_string()))?;
        Ok(Matchup {
            home: TeamAbbr(home.to_string()),
            away: TeamAbbr(away.to_string())
        })
    }
}

impl Display for Matchup {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}@{}", self.away, self.home)
    }
}