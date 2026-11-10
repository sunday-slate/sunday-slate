use std::collections::HashMap;
use std::sync::RwLock;

pub use nfl_data::InjuryDesignation;

use nfl_data::SeasonWeek;
use time::{Date, OffsetDateTime};

use crate::entries::NflPlayerId;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SlateInjuries {
    pub by_gsis: HashMap<NflPlayerId, InjuryDesignation>,
    pub report_date: Option<Date>,
    pub attempted_at: Option<OffsetDateTime>,
}

#[derive(Debug, Default)]
pub struct InjuryReports(RwLock<HashMap<SeasonWeek, SlateInjuries>>);

impl InjuryReports {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn report(&self, key: &SeasonWeek) -> Option<SlateInjuries> {
        self.0.read().ok()?.get(key).cloned()
    }

    /// Whole-swap: a publish replaces any prior report for the key outright.
    pub fn publish(&self, key: SeasonWeek, report: SlateInjuries) {
        if let Ok(mut cache) = self.0.write() {
            cache.insert(key, report);
        }
    }

    pub fn needs_refresh(
        &self,
        key: &SeasonWeek,
        now: OffsetDateTime,
        ttl: time::Duration,
    ) -> bool {
        let attempt = self
            .0
            .read()
            .ok()
            .and_then(|cache| cache.get(key).and_then(|report| report.attempted_at));
        match attempt {
            None => true,
            Some(attempted_at) => now - attempted_at > ttl,
        }
    }
}

pub struct InjuryChip {
    pub code: &'static str,
    pub title: &'static str,
    pub class: &'static str,
}

pub fn chip(designation: InjuryDesignation) -> InjuryChip {
    match designation {
        InjuryDesignation::Questionable => InjuryChip {
            code: "Q",
            title: "Questionable",
            class: "badge-warning badge-outline badge-xs",
        },
        InjuryDesignation::Doubtful => InjuryChip {
            code: "D",
            title: "Doubtful",
            class: "badge-error badge-xs",
        },
        InjuryDesignation::Out => InjuryChip {
            code: "O",
            title: "Out",
            class: "badge-error badge-xs",
        },
        InjuryDesignation::InjuredReserve => InjuryChip {
            code: "IR",
            title: "Injured Reserve",
            class: "badge-error badge-xs",
        },
    }
}

/// Deterministic demo mix (≈15% Q, 5% O, 8% IR, 2% D of the roster), stable
/// per player and across runs. Dev feed only.
#[doc(hidden)]
pub fn synthetic_dev<'a>(
    ids: impl Iterator<Item = &'a NflPlayerId>,
) -> HashMap<NflPlayerId, InjuryDesignation> {
    ids.filter_map(|id| {
        let roll = id.0.bytes().map(u32::from).sum::<u32>() % 100;
        let designation = if roll < 15 {
            InjuryDesignation::Questionable
        } else if roll < 20 {
            InjuryDesignation::Out
        } else if roll < 28 {
            InjuryDesignation::InjuredReserve
        } else if roll < 30 {
            InjuryDesignation::Doubtful
        } else {
            return None;
        };
        Some((id.clone(), designation))
    })
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> OffsetDateTime {
        time::macros::datetime!(2026-09-22 20:00 UTC)
    }

    fn week(season: u16, week: u8) -> SeasonWeek {
        SeasonWeek::new(nfl_data::Season(season), nfl_data::Week(week)).expect("valid fixture week")
    }

    fn report(entries: &[(NflPlayerId, InjuryDesignation)]) -> SlateInjuries {
        SlateInjuries {
            by_gsis: entries.iter().cloned().collect(),
            report_date: None,
            attempted_at: None,
        }
    }

    #[test]
    fn publish_overrides_the_whole_report_for_a_key() {
        let cache = InjuryReports::new();
        let key = week(2026, 1);
        cache.publish(
            key,
            report(&[(NflPlayerId("100".into()), InjuryDesignation::Out)]),
        );
        cache.publish(
            key,
            report(&[
                (NflPlayerId("200".into()), InjuryDesignation::Doubtful),
                (NflPlayerId("300".into()), InjuryDesignation::Questionable),
            ]),
        );

        let current = cache.report(&key).unwrap();
        assert!(!current.by_gsis.contains_key(&NflPlayerId("100".into())));
        assert_eq!(
            current.by_gsis.get(&NflPlayerId("200".into())),
            Some(&InjuryDesignation::Doubtful)
        );
        assert_eq!(current.by_gsis.len(), 2);
    }

    #[test]
    fn needs_refresh_tracks_ttl_since_last_attempt() {
        let cache = InjuryReports::new();
        let key = week(2026, 2);
        let now = now();
        let ttl = time::Duration::minutes(30);

        assert!(cache.needs_refresh(&key, now, ttl));

        cache.publish(
            key,
            SlateInjuries {
                attempted_at: Some(now - time::Duration::minutes(5)),
                ..Default::default()
            },
        );
        assert!(!cache.needs_refresh(&key, now, ttl));

        cache.publish(
            key,
            SlateInjuries {
                attempted_at: Some(now - time::Duration::minutes(31)),
                ..Default::default()
            },
        );
        assert!(cache.needs_refresh(&key, now, ttl));

        assert!(
            cache.needs_refresh(&week(2026, 3), now, ttl),
            "absent keys always need refresh"
        );
    }

    #[test]
    fn one_publish_stamps_multiple_keys() {
        let cache = InjuryReports::new();
        let now = now();
        let shared = SlateInjuries {
            by_gsis: HashMap::from([
                (NflPlayerId("100".into()), InjuryDesignation::Questionable),
                (NflPlayerId("200".into()), InjuryDesignation::InjuredReserve),
            ]),
            report_date: Some(time::macros::date!(2026 - 09 - 22)),
            attempted_at: Some(now),
        };
        let thursday = week(2026, 4);
        let sunday = week(2026, 5);
        cache.publish(thursday, shared.clone());
        cache.publish(sunday, shared.clone());

        assert_eq!(cache.report(&thursday), Some(shared.clone()));
        assert_eq!(cache.report(&sunday), Some(shared));
    }

    /// The same absolute week in two seasons is two reports: a 2025 week 1
    /// report must never answer for 2026 week 1.
    #[test]
    fn season_week_keeps_same_numbered_weeks_in_different_seasons_apart() {
        let cache = InjuryReports::new();
        let now = now();
        let older = week(2025, 1);
        let newer = week(2026, 1);
        let older_report = SlateInjuries {
            by_gsis: HashMap::from([(NflPlayerId("100".into()), InjuryDesignation::Out)]),
            report_date: Some(time::macros::date!(2025 - 09 - 02)),
            attempted_at: Some(now - time::Duration::minutes(10)),
        };
        let newer_report = SlateInjuries {
            by_gsis: HashMap::from([(NflPlayerId("100".into()), InjuryDesignation::Questionable)]),
            report_date: Some(time::macros::date!(2026 - 09 - 08)),
            attempted_at: Some(now - time::Duration::minutes(40)),
        };
        cache.publish(older, older_report.clone());
        cache.publish(newer, newer_report.clone());

        assert_eq!(cache.report(&older), Some(older_report));
        assert_eq!(cache.report(&newer), Some(newer_report));

        let ttl = time::Duration::minutes(30);
        assert!(!cache.needs_refresh(&older, now, ttl));
        assert!(cache.needs_refresh(&newer, now, ttl));
    }

    /// Two readers of the same week see the same whole-swap report and the
    /// same refresh state.
    #[test]
    fn season_week_two_readers_share_one_whole_swap_report() {
        let cache = InjuryReports::new();
        let key = week(2026, 7);
        let first = report(&[(NflPlayerId("100".into()), InjuryDesignation::Out)]);
        let second = report(&[
            (NflPlayerId("200".into()), InjuryDesignation::Doubtful),
            (NflPlayerId("300".into()), InjuryDesignation::InjuredReserve),
        ]);
        cache.publish(key, first);
        cache.publish(key, second.clone());

        let reader_one = cache.report(&key);
        let reader_two = cache.report(&key);
        assert_eq!(reader_one, Some(second.clone()));
        assert_eq!(reader_two, Some(second.clone()));
        assert_eq!(reader_one, reader_two);

        let now = now();
        let ttl = time::Duration::minutes(30);
        assert_eq!(
            cache.needs_refresh(&key, now, ttl),
            cache.needs_refresh(&key, now, ttl)
        );
        assert_eq!(
            cache.report(&key).and_then(|report| report.attempted_at),
            cache.report(&key).and_then(|report| report.attempted_at)
        );
    }
}
