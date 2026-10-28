use time::OffsetDateTime;

use nfl_model::SeasonWeek;

/// A shared, named slate: stored metadata without any games.
///
/// Ordinary construction reads metadata from its database row; games are
/// retrieved separately and never embedded. The optional kickoff is the
/// slate's earliest known game kickoff, stored rather than recomputed.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Slate {
    /// Derived identifier — the same numeric value in every database; see
    /// [`SlateId`] for the derivation from the slate's week and kind.
    pub id: SlateId,
    /// The exact NFL week this slate belongs to.
    pub season_week: SeasonWeek,
    /// Canonical name, e.g. `Week 1`, `Thanksgiving`, or `Wild Card`.
    pub name: String,
    /// The slate's earliest known kickoff in UTC, or `None` when unknown.
    pub kickoff: Option<OffsetDateTime>,
}

/// Opaque wrapper around a slate's deterministic identity.
///
/// Derived from the slate's NFL week and kind as `SeasonWeek.encode() * 10 +
/// kind`, so the same slate carries the identical numeric value in every
/// database. The kind digit is `1` for main slates — regular-season `Week 1`
/// through `Week 18` and the postseason `Wild Card`, `Divisional`, and
/// `Championship` slates — and `0` for the holiday slates, `Thanksgiving`
/// and `Christmas`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, sqlx::Type)]
#[sqlx(transparent)]
pub struct SlateId(pub i64);

/// Tentative slate content produced by the pure membership policy and
/// consumed by storage reconciliation. Not public API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SlateProposal {
    pub id: SlateId,
    pub name: String,
    pub kickoff: Option<OffsetDateTime>,
    pub game_ids: Vec<String>,
}
