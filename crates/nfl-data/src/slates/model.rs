use time::OffsetDateTime;

use nfl_model::SeasonWeek;

/// A shared, named slate: stored metadata without any games.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, sqlx::FromRow)]
pub struct Slate {
    /// Derived identifier; see [`SlateId`].
    pub id: SlateId,
    /// The exact NFL week this slate belongs to.
    pub season_week: SeasonWeek,
    /// Canonical name, e.g. `Week 1`, `Thanksgiving`, or `Wild Card`.
    pub name: String,
    /// The slate's earliest known kickoff in UTC, or `None` when unknown.
    pub kickoff: Option<OffsetDateTime>,
    /// Whether its games span more than one Eastern calendar date, e.g. a
    /// Saturday/Sunday postseason weekend.
    pub multiday: bool,
}

impl Slate {
    /// Where the slate sits relative to `now`, by Eastern calendar dates.
    ///
    /// Unknown kickoff means [`SlateStatus::Upcoming`]. A known slate is live
    /// from its kickoff through the end of its kickoff date (and the following
    /// date when [`multiday`](Self::multiday)), then finished.
    pub fn status(&self, now: OffsetDateTime) -> SlateStatus {
        let Some(kickoff) = self.kickoff else {
            return SlateStatus::Upcoming;
        };
        let kickoff = crate::to_eastern(kickoff);
        let now = crate::to_eastern(now);
        if (now.date() - kickoff.date()).whole_days() > i64::from(self.multiday) {
            SlateStatus::Finished
        } else if now >= kickoff {
            SlateStatus::Live
        } else {
            SlateStatus::Upcoming
        }
    }

    /// Whether the slate has not kicked off yet.
    pub fn upcoming(&self, now: OffsetDateTime) -> bool {
        self.status(now) == SlateStatus::Upcoming
    }

    /// Whether the slate has kicked off but is not finished yet.
    pub fn live(&self, now: OffsetDateTime) -> bool {
        self.status(now) == SlateStatus::Live
    }

    /// Whether the slate's games are over.
    pub fn finished(&self, now: OffsetDateTime) -> bool {
        self.status(now) == SlateStatus::Finished
    }

    /// Whether the slate has kicked off: a known kickoff at or before `now`.
    pub fn locked(&self, now: OffsetDateTime) -> bool {
        !self.upcoming(now)
    }
}

/// Where a slate sits in time, decided from its kickoff and the clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlateStatus {
    Upcoming,
    Live,
    Finished,
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
    pub multiday: bool,
    pub game_ids: Vec<String>,
}

/// Whether `kickoffs` fall on more than one Eastern calendar date; an empty or
/// single-date set is one day.
pub(crate) fn spans_multiple_days(kickoffs: impl IntoIterator<Item = OffsetDateTime>) -> bool {
    let mut dates = kickoffs
        .into_iter()
        .map(|kickoff| crate::to_eastern(kickoff).date());
    let Some(first) = dates.next() else {
        return false;
    };
    dates.any(|date| date != first)
}

#[cfg(test)]
mod tests {
    use time::macros::datetime;

    use super::{Slate, SlateId, SlateStatus};
    use crate::{Season, SeasonType, SeasonWeek, Week};

    fn slate(kickoff: Option<time::OffsetDateTime>, multiday: bool) -> Slate {
        Slate {
            id: SlateId(1),
            season_week: SeasonWeek {
                season: Season(2026),
                season_type: SeasonType::Reg,
                week: Week(1),
            },
            name: "Week 1".into(),
            kickoff,
            multiday,
        }
    }

    fn assert_status(slate: &Slate, now: time::OffsetDateTime, expected: SlateStatus) {
        assert_eq!(slate.status(now), expected, "status at {now}");
        assert_eq!(slate.upcoming(now), expected == SlateStatus::Upcoming);
        assert_eq!(slate.live(now), expected == SlateStatus::Live);
        assert_eq!(slate.finished(now), expected == SlateStatus::Finished);
        assert_eq!(slate.locked(now), expected != SlateStatus::Upcoming);
    }

    #[test]
    fn single_day_slate_lives_through_its_eastern_date() {
        let slate = slate(Some(datetime!(2026-09-13 17:00 UTC)), false);
        assert_status(
            &slate,
            datetime!(2026-09-13 16:59:59 UTC),
            SlateStatus::Upcoming,
        );
        assert_status(&slate, datetime!(2026-09-13 17:00 UTC), SlateStatus::Live);
        assert_status(
            &slate,
            datetime!(2026-09-14 03:59:59 UTC),
            SlateStatus::Live,
        );
        assert_status(
            &slate,
            datetime!(2026-09-14 04:00 UTC),
            SlateStatus::Finished,
        );
    }

    #[test]
    fn multiday_slate_lives_through_the_following_eastern_date() {
        let slate = slate(Some(datetime!(2026-09-13 17:00 UTC)), true);
        assert_status(&slate, datetime!(2026-09-13 17:00 UTC), SlateStatus::Live);
        assert_status(
            &slate,
            datetime!(2026-09-15 03:59:59 UTC),
            SlateStatus::Live,
        );
        assert_status(
            &slate,
            datetime!(2026-09-15 04:00 UTC),
            SlateStatus::Finished,
        );
    }

    #[test]
    fn evening_kickoff_uses_eastern_not_utc_dates() {
        // Sunday Nov 8, 8 PM EST: UTC has already rolled over to Monday.
        let slate = slate(Some(datetime!(2026-11-09 01:00 UTC)), false);
        assert_status(
            &slate,
            datetime!(2026-11-09 04:59:59 UTC),
            SlateStatus::Live,
        );
        assert_status(
            &slate,
            datetime!(2026-11-09 05:00 UTC),
            SlateStatus::Finished,
        );
    }

    #[test]
    fn supplied_offsets_do_not_change_the_classification() {
        let utc = slate(Some(datetime!(2026-09-13 17:00 UTC)), false);
        let offset = slate(Some(datetime!(2026-09-13 13:00 -4)), false);
        for (now_utc, now_offset) in [
            (
                datetime!(2026-09-13 16:59:59 UTC),
                datetime!(2026-09-13 12:59:59 -4),
            ),
            (
                datetime!(2026-09-13 17:00 UTC),
                datetime!(2026-09-13 13:00 -4),
            ),
            (
                datetime!(2026-09-14 03:59:59 UTC),
                datetime!(2026-09-13 23:59:59 -4),
            ),
            (
                datetime!(2026-09-14 04:00 UTC),
                datetime!(2026-09-14 00:00 -4),
            ),
        ] {
            assert_eq!(offset.status(now_offset), utc.status(now_utc));
        }
    }

    #[test]
    fn unknown_kickoff_is_upcoming_and_unlocked() {
        for multiday in [false, true] {
            let slate = slate(None, multiday);
            assert_status(
                &slate,
                datetime!(2030-01-01 00:00 UTC),
                SlateStatus::Upcoming,
            );
        }
    }
}
