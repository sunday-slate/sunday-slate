//! Pure view assembly for the owner's open-contests section on the team
//! detail page: contests still worth acting on, soonest first.

use std::collections::{HashMap, HashSet};

use time::OffsetDateTime;

use crate::contests::service::{format_kickoff, time_until};
use crate::entries::EntryId;
use nfl_data::slates::{Slate, SlateId, SlateStatus};

/// What an open-contest row offers the owner.
pub enum OpenAction {
    /// Entered, pre-kickoff: the row links to the lineup at `/entries/{id}`.
    ViewEntry(EntryId),
    /// Not entered, pre-kickoff: a button into `/contests/{id}/draft-entry`.
    /// `true` when a partial draft exists to resume.
    Draft(bool),
    /// Entered and live: the row links to `/contests/{id}`.
    Live,
}

/// One row in the open-contests section.
pub struct OpenContestRow {
    pub contest: SlateId,
    pub name: String,
    /// Pre-kickoff: "Kicks off Sun, Sep 7 · 1:00 PM ET — locks in 3 days".
    /// `None` exactly when the contest is live.
    pub status: Option<String>,
    pub action: OpenAction,
}

/// Select and order the owner's open contests: published slate, not
/// finished, and either entered or still open for entry. Live contests the
/// team did not enter are dropped — nothing there is actionable. Soonest
/// kickoff first, ties by contest id.
pub fn open_contests(
    contests: &[Slate],
    entries: &HashMap<SlateId, EntryId>,
    resumable: &HashSet<SlateId>,
    now: OffsetDateTime,
) -> Vec<OpenContestRow> {
    let mut rows: Vec<(OffsetDateTime, OpenContestRow)> = contests
        .iter()
        .filter_map(|c| {
            let first = nfl_data::to_eastern(c.kickoff?);
            let entry = entries.get(&c.id).copied();
            let (status, action) = match c.status(now) {
                SlateStatus::Finished => return None,
                SlateStatus::Live => {
                    entry?;
                    (None, OpenAction::Live)
                }
                SlateStatus::Upcoming => {
                    let status = format!(
                        "Kicks off {} ET — locks {}",
                        format_kickoff(first),
                        time_until(now, first)
                    );
                    let action = match entry {
                        Some(id) => OpenAction::ViewEntry(id),
                        None => OpenAction::Draft(resumable.contains(&c.id)),
                    };
                    (Some(status), action)
                }
            };
            Some((
                first,
                OpenContestRow {
                    contest: c.id,
                    name: c.name.clone(),
                    status,
                    action,
                },
            ))
        })
        .collect();
    rows.sort_by_key(|(first, row)| (*first, row.contest.0));
    rows.into_iter().map(|(_, r)| r).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use nfl_data::{Season, SeasonWeek, Week};
    use time::macros::datetime;

    fn slate(id: i64, name: &str, kickoff: OffsetDateTime, multiday: bool) -> Slate {
        Slate {
            id: SlateId(id),
            season_week: SeasonWeek {
                season: Season(2026),
                week: Week(1),
            },
            name: name.to_string(),
            kickoff: Some(kickoff),
            multiday,
        }
    }

    /// A one-day slate kicking off Sun, Sep 7 2025 at 1:00 PM ET.
    fn week1() -> Slate {
        let k = datetime!(2025 - 09 - 07 13:00 -4);
        slate(1, "Week 1", k, false)
    }

    /// Three days before week1's kickoff.
    const NOW: OffsetDateTime = datetime!(2025 - 09 - 04 13:00 -4);

    fn no_entries() -> HashMap<SlateId, EntryId> {
        HashMap::new()
    }

    #[test]
    fn upcoming_entered_links_to_the_entry() {
        let entries = HashMap::from([(SlateId(1), EntryId(42))]);
        let rows = open_contests(&[week1()], &entries, &HashSet::new(), NOW);
        assert_eq!(rows.len(), 1);
        assert!(matches!(rows[0].action, OpenAction::ViewEntry(EntryId(42))));
        assert_eq!(
            rows[0].status.as_deref(),
            Some("Kicks off Sun, Sep 7 · 1:00 PM ET — locks in 3 days")
        );
    }

    #[test]
    fn stored_utc_kickoff_renders_eastern_wall_clock() {
        // 17:00 UTC is 1:00 PM EDT; a raw UTC render would say 5:00 PM.
        let k = datetime!(2025-09-07 17:00 UTC);
        let rows = open_contests(
            &[slate(1, "Week 1", k, false)],
            &no_entries(),
            &HashSet::new(),
            datetime!(2025-09-04 12:00 UTC),
        );
        assert_eq!(
            rows[0].status.as_deref(),
            Some("Kicks off Sun, Sep 7 · 1:00 PM ET — locks in 3 days")
        );
    }

    #[test]
    fn upcoming_unentered_offers_the_draft_editor() {
        let rows = open_contests(&[week1()], &no_entries(), &HashSet::new(), NOW);
        assert_eq!(rows.len(), 1);
        assert!(matches!(rows[0].action, OpenAction::Draft(false)));
    }

    #[test]
    fn a_partial_draft_flips_the_action_to_resume() {
        let resumable = HashSet::from([SlateId(1)]);
        let rows = open_contests(&[week1()], &no_entries(), &resumable, NOW);
        assert!(matches!(rows[0].action, OpenAction::Draft(true)));
    }

    #[test]
    fn live_entered_watches_the_contest() {
        // Kickoff has passed but the slate's last day is today: live.
        let entries = HashMap::from([(SlateId(1), EntryId(42))]);
        let rows = open_contests(
            &[week1()],
            &entries,
            &HashSet::new(),
            datetime!(2025 - 09 - 07 16:00 -4),
        );
        assert_eq!(rows.len(), 1);
        assert!(matches!(rows[0].action, OpenAction::Live));
        assert_eq!(
            rows[0].status, None,
            "live rows carry a badge, not a status line"
        );
    }

    #[test]
    fn live_unentered_is_dropped() {
        let rows = open_contests(
            &[week1()],
            &no_entries(),
            &HashSet::new(),
            datetime!(2025 - 09 - 07 16:00 -4),
        );
        assert!(rows.is_empty());
    }

    #[test]
    fn finished_contests_are_dropped_even_when_entered() {
        let entries = HashMap::from([(SlateId(1), EntryId(42))]);
        let rows = open_contests(
            &[week1()],
            &entries,
            &HashSet::new(),
            datetime!(2025 - 09 - 08 12:00 -4),
        );
        assert!(rows.is_empty());
    }

    #[test]
    fn unpublished_slates_are_dropped() {
        let undated = Slate {
            id: SlateId(9),
            season_week: SeasonWeek {
                season: Season(2026),
                week: Week(9),
            },
            name: "Week 9".to_string(),
            kickoff: None,
            multiday: false,
        };
        let rows = open_contests(&[undated], &no_entries(), &HashSet::new(), NOW);
        assert!(rows.is_empty());
    }

    #[test]
    fn rows_order_by_soonest_kickoff() {
        // Thanksgiving week: Sunday slate listed after Thursday's.
        let thu = datetime!(2025 - 11 - 27 13:00 -5);
        let sun = datetime!(2025 - 11 - 30 13:00 -5);
        let rows = open_contests(
            &[
                slate(1, "Sunday Slate", sun, false),
                slate(2, "Thanksgiving", thu, false),
            ],
            &no_entries(),
            &HashSet::new(),
            datetime!(2025 - 11 - 25 12:00 -5),
        );
        let order: Vec<i64> = rows.iter().map(|r| r.contest.0).collect();
        assert_eq!(order, vec![2, 1]);
    }
}
