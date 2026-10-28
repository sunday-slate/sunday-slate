use time::OffsetDateTime;

use nfl_model::SeasonWeek;

/// A shared, named slate: stored metadata without any games.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Slate {
    /// Derived identifier; see [`SlateId`].
    pub id: SlateId,
    /// The exact NFL week this slate belongs to.
    pub season_week: SeasonWeek,
    /// Canonical name, e.g. `Week 1`, `Thanksgiving`, or `Wild Card`.
    pub name: String,
    /// The slate's earliest known kickoff in UTC, or `None` when unknown.
    pub kickoff: Option<OffsetDateTime>,
}

/// Opaque deterministic slate id: `SeasonWeek.encode() * 10 + kind` from the
/// slate's week and kind, identical in every database. The kind is `1` for
/// main slates — regular weeks and the postseason slates — and `0` for the
/// holiday slates, `Thanksgiving` and `Christmas`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, sqlx::Type)]
#[sqlx(transparent)]
pub struct SlateId(pub i64);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SlateProposal {
    pub id: SlateId,
    pub name: String,
    pub kickoff: Option<OffsetDateTime>,
    pub game_ids: Vec<String>,
}
