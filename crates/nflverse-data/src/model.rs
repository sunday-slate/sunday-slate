use std::fmt;

use time::OffsetDateTime;

use crate::error::NflDataError;

pub use nfl_model::{
    Game, Player, PlayerWeekStats, RosterEntry, Season, SeasonType, TeamAbbr, TeamWeekStats, Week,
    WeeklyRosterEntry,
};

/// One player's summed PPR points and games played over a span of weeks.
#[derive(Debug, Clone, PartialEq)]
pub struct PlayerSeasonTotals {
    pub gsis_id: String,
    pub fantasy_points_ppr: f64,
    pub games: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dataset {
    Schedules,
    Players,
    Rosters,
    WeeklyRosters,
    PlayerWeekStats,
    Pbp,
}

impl Dataset {
    pub(crate) const ALL: [Dataset; 6] = [
        Dataset::Schedules,
        Dataset::Players,
        Dataset::Rosters,
        Dataset::WeeklyRosters,
        Dataset::PlayerWeekStats,
        Dataset::Pbp,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Dataset::Schedules => "schedules",
            Dataset::Players => "players",
            Dataset::Rosters => "rosters",
            Dataset::WeeklyRosters => "weekly_rosters",
            Dataset::PlayerWeekStats => "player_week_stats",
            Dataset::Pbp => "team_week_stats",
        }
    }

    pub(crate) fn release_tag(self) -> &'static str {
        match self {
            Dataset::Schedules => "schedules",
            Dataset::Players => "players",
            Dataset::Rosters => "rosters",
            Dataset::WeeklyRosters => "weekly_rosters",
            Dataset::PlayerWeekStats => "stats_player",
            Dataset::Pbp => "pbp",
        }
    }
}
#[derive(Debug, Clone, PartialEq)]
pub struct DatasetFreshness {
    pub dataset: Dataset,
    pub assets: u32,
    pub last_synced_at: Option<OffsetDateTime>,
}

#[derive(Debug)]
pub struct SyncReport {
    pub datasets: Vec<DatasetReport>,
}

#[derive(Debug)]
pub struct DatasetReport {
    pub dataset: Dataset,
    pub status: DatasetStatus,
}

/// Outcome of syncing one dataset.
#[derive(Debug)]
pub enum DatasetStatus {
    Updated {
        assets: u32,
        rows: u64,
    },
    Unchanged,
    /// The dataset's first per-asset error. For per-season datasets this
    /// does not mean nothing changed: other seasons' assets in the same
    /// dataset may have updated successfully before this error occurred.
    Failed(NflDataError),
}

impl SyncReport {
    pub fn all_ok(&self) -> bool {
        self.datasets
            .iter()
            .all(|d| !matches!(d.status, DatasetStatus::Failed(_)))
    }
}

impl fmt::Display for SyncReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for d in &self.datasets {
            match &d.status {
                DatasetStatus::Updated { assets, rows } => writeln!(
                    f,
                    "{:<18} updated ({assets} assets, {rows} rows)",
                    d.dataset.name()
                )?,
                DatasetStatus::Unchanged => writeln!(f, "{:<18} unchanged", d.dataset.name())?,
                DatasetStatus::Failed(e) => writeln!(f, "{:<18} FAILED: {e}", d.dataset.name())?,
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_ok_is_false_when_any_dataset_failed() {
        let report = SyncReport {
            datasets: vec![
                DatasetReport {
                    dataset: Dataset::Schedules,
                    status: DatasetStatus::Updated {
                        assets: 1,
                        rows: 10,
                    },
                },
                DatasetReport {
                    dataset: Dataset::Players,
                    status: DatasetStatus::Failed(NflDataError::MissingAsset {
                        release_tag: "players".into(),
                        name: "players.csv".into(),
                    }),
                },
            ],
        };
        assert!(!report.all_ok());
        let rendered = report.to_string();
        assert!(rendered.contains("schedules"));
        assert!(rendered.contains("FAILED"));
    }
}
