use std::collections::HashMap;
use std::sync::RwLock;

pub use nfl_data::InjuryDesignation;

use nfl_data::{Season, SeasonType, Week};
use time::{Date, OffsetDateTime};

use crate::entries::NflPlayerId;

pub type SlateKey = (Season, Week, SeasonType);

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SlateInjuries {
    pub by_gsis: HashMap<NflPlayerId, InjuryDesignation>,
    pub report_date: Option<Date>,
    pub attempted_at: Option<OffsetDateTime>,
}

#[derive(Debug, Default)]
pub struct InjuryReports(RwLock<HashMap<SlateKey, SlateInjuries>>);

impl InjuryReports {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn report(&self, key: &SlateKey) -> Option<SlateInjuries> {
        self.0.read().ok()?.get(key).cloned()
    }

    /// Whole-swap: a publish replaces any prior report for the key outright.
    pub fn publish(&self, key: SlateKey, report: SlateInjuries) {
        if let Ok(mut cache) = self.0.write() {
            cache.insert(key, report);
        }
    }

    pub fn needs_refresh(&self, key: &SlateKey, now: OffsetDateTime, ttl: time::Duration) -> bool {
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
        let week = Week(1);
        let season = Season(2026);
        let key: SlateKey = (season, week, SeasonType::Reg);
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
        let key: SlateKey = (Season(2026), Week(2), SeasonType::Reg);
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
            cache.needs_refresh(&(Season(2026), Week(3), SeasonType::Reg), now, ttl),
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
        let thursday: SlateKey = (Season(2026), Week(4), SeasonType::Reg);
        let sunday: SlateKey = (Season(2026), Week(5), SeasonType::Reg);
        cache.publish(thursday, shared.clone());
        cache.publish(sunday, shared.clone());

        assert_eq!(cache.report(&thursday), Some(shared.clone()));
        assert_eq!(cache.report(&sunday), Some(shared));
    }
}
