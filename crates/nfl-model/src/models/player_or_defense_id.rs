use crate::{NflPlayerId, TeamAbbr};

/// The identifier a pick carries: an NFL player or a team defense.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PlayerOrDefenseId {
    Player(NflPlayerId),
    Defense(TeamAbbr),
}

impl std::fmt::Display for PlayerOrDefenseId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Player(id) => f.write_str(id.as_ref()),
            Self::Defense(team) => f.write_str(team.as_str()),
        }
    }
}
