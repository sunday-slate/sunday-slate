use time::OffsetDateTime;

use nfl_model::SeasonWeek;

/// A shared, named slate: database-assigned metadata without any games.
///
/// Ordinary construction reads metadata from its database row; games are
/// retrieved separately and never embedded. The optional kickoff is the
/// slate's earliest known game kickoff, stored rather than recomputed.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Slate {
    /// Database-assigned identifier. Auto-incrementing, so it identifies a row
    /// in one database and is not a deterministic id shared across databases.
    pub id: SlateId,
    /// The exact NFL week this slate belongs to.
    pub season_week: SeasonWeek,
    /// Canonical name, e.g. `Week 1`, `Thanksgiving`, or `Wild Card`.
    pub name: String,
    /// The slate's earliest known kickoff in UTC, or `None` when unknown.
    pub kickoff: Option<OffsetDateTime>,
}

/// Opaque wrapper around a slate's auto-incrementing database integer.
///
/// Not a deterministic identifier: the same named slate gets a different id
/// in each database.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, sqlx::Type)]
#[sqlx(transparent)]
pub struct SlateId(pub i64);

/// Tentative slate content produced by the pure membership policy and consumed
/// by storage reconciliation in a later task. Not public API.
#[cfg_attr(not(test), expect(dead_code))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SlateProposal {
    pub name: String,
    pub kickoff: Option<OffsetDateTime>,
    pub game_ids: Vec<String>,
}
