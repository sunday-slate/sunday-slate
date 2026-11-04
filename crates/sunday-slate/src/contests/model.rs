use nfl_data::slates::SlateId;
use nfl_data::{Season, SeasonType, SeasonWeek};

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

    /// The main slate this contest scores against: its name's week in the
    /// default season, encoded as an nfl-data `SlateId`, kind digit `1` for a
    /// main slate.
    pub fn slate_id(&self) -> Option<SlateId> {
        let Rule::Main { week } = self.rule()? else {
            return None;
        };
        let week = SeasonWeek::new(Season::default(), SeasonType::Reg, week)?;
        Some(SlateId(i64::from(week.encode()) * 10 + 1))
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
