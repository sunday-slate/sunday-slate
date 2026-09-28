//! Pure home dispatch: which contest is the "Current Contest".

use time::OffsetDateTime;

use crate::contests::ContestId;
use crate::contests::service::{ContestState, SlateContest, contest_state};

/// The contest `GET /` redirects to, and the bottom bar's Current tab.
///
/// Resolution: the soonest live contest, else the soonest dated upcoming
/// contest, else the lowest-id undated (unpublished-slate) contest, else the
/// most recently finished contest — "this week" lingers until the next
/// contest is created. `None` means the season has no contests at all: the
/// Current tab hides and `/` falls back to the standings.
pub fn current_contest(contests: &[SlateContest], now: OffsetDateTime) -> Option<ContestId> {
    let mut live: Vec<(OffsetDateTime, ContestId)> = Vec::new();
    let mut upcoming: Vec<(OffsetDateTime, ContestId)> = Vec::new();
    let mut undated: Vec<ContestId> = Vec::new();
    let mut finished: Vec<(OffsetDateTime, ContestId)> = Vec::new();
    for c in contests {
        match contest_state(c.first, c.last, now) {
            ContestState::Live => {
                live.push((c.first.expect("live contests have kickoffs"), c.id));
            }
            ContestState::Upcoming => match c.first {
                Some(first) => upcoming.push((first, c.id)),
                None => undated.push(c.id),
            },
            ContestState::Finished => {
                finished.push((c.last.expect("finished contests have kickoffs"), c.id));
            }
        }
    }
    live.sort_unstable();
    upcoming.sort_unstable();
    undated.sort_unstable();
    finished.sort_unstable_by_key(|&(last, id)| (last, std::cmp::Reverse(id)));

    live.first()
        .map(|&(_, id)| id)
        .or(upcoming.first().map(|&(_, id)| id))
        .or(undated.first().copied())
        .or(finished.last().map(|&(_, id)| id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::{date, datetime};

    fn kickoff(date: time::Date) -> OffsetDateTime {
        date.with_hms(13, 0, 0)
            .expect("valid time")
            .assume_offset(nfl_data::eastern_offset(date))
    }

    fn contest(id: i64, first: Option<time::Date>, last: Option<time::Date>) -> SlateContest {
        SlateContest {
            id: ContestId(id),
            name: format!("Contest {id}"),
            first: first.map(kickoff),
            last: last.map(kickoff),
        }
    }

    fn one_day(id: i64, day: time::Date) -> SlateContest {
        contest(id, Some(day), Some(day))
    }

    #[test]
    fn a_lone_live_contest_is_current() {
        let target = current_contest(
            &[
                one_day(1, date!(2025 - 09 - 07)), // finished
                one_day(2, date!(2025 - 09 - 14)), // live at `now`
                one_day(3, date!(2025 - 09 - 21)), // upcoming, next week
            ],
            datetime!(2025 - 09 - 14 20:00 UTC),
        );
        assert_eq!(target, Some(ContestId(2)));
    }

    #[test]
    fn the_soonest_upcoming_contest_beats_results() {
        let target = current_contest(
            &[
                one_day(1, date!(2025 - 09 - 07)), // finished
                one_day(3, date!(2025 - 09 - 21)), // later upcoming
                one_day(2, date!(2025 - 09 - 14)), // next upcoming
            ],
            datetime!(2025 - 09 - 10 12:00 UTC),
        );
        assert_eq!(target, Some(ContestId(2)));
    }

    #[test]
    fn a_holiday_week_resolves_to_its_soonest_slate() {
        // Thanksgiving week carries two upcoming slates; Current is the
        // soonest (Thursday). The Contests tab covers the sibling.
        let target = current_contest(
            &[
                one_day(1, date!(2025 - 11 - 30)), // Sunday main slate
                one_day(2, date!(2025 - 11 - 27)), // Thanksgiving
                one_day(3, date!(2025 - 12 - 07)), // next week
            ],
            datetime!(2025 - 11 - 25 12:00 UTC),
        );
        assert_eq!(target, Some(ContestId(2)));
    }

    #[test]
    fn a_live_contest_beats_an_upcoming_one() {
        // Thanksgiving evening: Thursday's contest is live, Sunday's upcoming.
        let target = current_contest(
            &[
                one_day(1, date!(2025 - 11 - 30)),
                one_day(2, date!(2025 - 11 - 27)),
            ],
            datetime!(2025 - 11 - 27 23:00 UTC),
        );
        assert_eq!(target, Some(ContestId(2)));
    }

    #[test]
    fn two_simultaneous_live_contests_pick_the_lowest_id() {
        let target = current_contest(
            &[
                one_day(2, date!(2025 - 11 - 27)),
                one_day(1, date!(2025 - 11 - 27)),
            ],
            datetime!(2025 - 11 - 27 23:00 UTC),
        );
        assert_eq!(target, Some(ContestId(1)));
    }

    #[test]
    fn a_finished_contest_with_no_successor_stays_current() {
        // "Between weeks" is not a state: the most recent finished contest
        // holds the Current slot until the next one is created.
        let target = current_contest(
            &[
                one_day(1, date!(2025 - 09 - 07)),
                one_day(2, date!(2025 - 09 - 14)),
            ],
            datetime!(2025 - 09 - 20 12:00 UTC),
        );
        assert_eq!(target, Some(ContestId(2)));
    }

    #[test]
    fn two_simultaneous_finished_contests_pick_the_lowest_id() {
        let target = current_contest(
            &[
                one_day(2, date!(2025 - 09 - 07)),
                one_day(1, date!(2025 - 09 - 07)),
            ],
            datetime!(2025 - 09 - 20 12:00 UTC),
        );
        assert_eq!(target, Some(ContestId(1)));
    }

    #[test]
    fn a_dated_upcoming_contest_beats_an_undated_one() {
        let target = current_contest(
            &[contest(9, None, None), one_day(2, date!(2025 - 09 - 14))],
            datetime!(2025 - 09 - 10 12:00 UTC),
        );
        assert_eq!(target, Some(ContestId(2)));
        let target = current_contest(
            &[contest(9, None, None)],
            datetime!(2025 - 09 - 10 12:00 UTC),
        );
        assert_eq!(target, Some(ContestId(9)));
    }

    #[test]
    fn several_undated_contests_pick_the_lowest_id() {
        let target = current_contest(
            &[contest(9, None, None), contest(8, None, None)],
            datetime!(2025 - 09 - 10 12:00 UTC),
        );
        assert_eq!(target, Some(ContestId(8)));
    }

    #[test]
    fn no_contests_is_none() {
        assert_eq!(
            current_contest(&[], datetime!(2025 - 09 - 10 12:00 UTC)),
            None
        );
    }
}
