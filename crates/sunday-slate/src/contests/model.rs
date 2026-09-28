use crate::contests::rule::Rule;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Contest {
    pub id: ContestId,
    pub name: String,
}

impl Contest {
    /// The resolution rule this contest's name encodes. `None` only if the row
    /// carries a name outside the canonical set (never for setup-created rows).
    pub fn rule(&self) -> Option<Rule> {
        Rule::from_name(&self.name)
    }
}

/// A contest's id as an opaque token. Contest names are display strings (and
/// the Rule encoding) — identity is this id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, sqlx::Type)]
#[sqlx(transparent)]
pub struct ContestId(pub i64);

/// A game's nflverse id as an opaque token (e.g. "2025_01_BUF_KC"). Wraps
/// nfl-data's `String` at the boundary — the identity of a slate's games.
#[derive(Debug, Clone, PartialEq, Eq, Hash, sqlx::Type)]
#[sqlx(transparent)]
pub struct NflGameId(pub String);
