use std::fmt;

use time::{Date, OffsetDateTime};

use crate::error::NflDataError;

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    serde::Serialize,
    serde::Deserialize,
    sqlx::Type,
)]
#[sqlx(transparent)]
#[serde(transparent)]
pub struct Season(pub u16);

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    serde::Serialize,
    serde::Deserialize,
    sqlx::Type,
)]
#[sqlx(transparent)]
#[serde(transparent)]
pub struct Week(pub u8);

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, sqlx::Type)]
#[sqlx(transparent)]
#[serde(transparent)]
pub struct TeamAbbr(pub String);

/// Stored as TEXT "REG"/"POST". Upstream playoff game types (WC/DIV/CON/SB)
/// map to Post at ingest.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, sqlx::Type,
)]
#[sqlx(rename_all = "UPPERCASE")]
#[serde(rename_all = "UPPERCASE")]
pub enum SeasonType {
    Reg,
    Post,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Game {
    pub gsis_game_id: String,
    pub season: Season,
    pub week: Week,
    pub season_type: SeasonType,
    /// UTC. None when upstream omits the kickoff time (some historical games).
    pub kickoff: Option<OffsetDateTime>,
    pub home_team: TeamAbbr,
    pub away_team: TeamAbbr,
    pub home_score: Option<i32>,
    pub away_score: Option<i32>,
}

impl Game {
    /// Kickoff in US Eastern local wall-clock time, or `None` when the kickoff
    /// is unknown. The stored kickoff is an instant, so DST is resolved from
    /// the transition instants for its year.
    pub fn kickoff_eastern(&self) -> Option<OffsetDateTime> {
        self.kickoff.map(crate::eastern::to_eastern)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Player {
    pub gsis_id: String,
    pub espn_id: Option<String>,
    pub full_name: String,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub position: Option<String>,
    pub latest_team: Option<TeamAbbr>,
    pub status: Option<String>,
    pub birth_date: Option<Date>,
    pub headshot_url: Option<String>,
}

/// How a page names one pick: the display facts every consumer of a gsis id
/// needs, resolved from whichever source carries the player.
#[derive(Debug, Clone, PartialEq)]
pub struct PlayerIdentity {
    pub name: String,
    pub team: Option<TeamAbbr>,
    pub position: Option<String>,
    /// Player photo URL from the `players` release. `None` for picks named
    /// only from weekly rosters (that source carries no headshot).
    pub headshot_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RosterEntry {
    pub season: Season,
    pub team: TeamAbbr,
    /// None for players without a GSIS assignment yet (upstream leaves it empty).
    pub gsis_id: Option<String>,
    pub full_name: String,
    pub position: Option<String>,
    pub jersey_number: Option<u16>,
    pub status: String,
}

/// One player's place on a team's roster in a single week. Unlike
/// [`RosterEntry`], this survives mid-season trades: a player appears under
/// each team for the weeks he was actually there.
#[derive(Debug, Clone, PartialEq)]
pub struct WeeklyRosterEntry {
    pub season: Season,
    pub week: Week,
    pub team: TeamAbbr,
    /// None for players without a GSIS assignment yet (upstream leaves it empty).
    pub gsis_id: Option<String>,
    pub espn_id: Option<String>,
    pub full_name: String,
    /// Surname with any generational suffix already stripped by upstream
    /// ("Dante Fowler Jr." -> "Fowler"). Sort candidate lists on this.
    pub last_name: Option<String>,
    pub position: Option<String>,
    pub status: String,
}

/// One player's summed PPR points and games played over a span of weeks.
#[derive(Debug, Clone, PartialEq)]
pub struct PlayerSeasonTotals {
    pub gsis_id: String,
    pub fantasy_points_ppr: f64,
    pub games: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PlayerWeekStats {
    pub season: Season,
    pub week: Week,
    pub season_type: SeasonType,
    pub gsis_id: String,
    pub team: TeamAbbr,
    pub opponent: Option<TeamAbbr>,
    pub completions: u32,
    pub attempts: u32,
    pub passing_yards: i32,
    pub passing_tds: u32,
    pub passing_interceptions: u32,
    pub rushing_attempts: u32,
    pub rushing_yards: i32,
    pub rushing_tds: u32,
    pub targets: u32,
    pub receptions: u32,
    pub receiving_yards: i32,
    pub receiving_tds: u32,
    /// Sum of upstream rushing/receiving/sack fumbles lost.
    pub fumbles_lost: u32,
    /// Sum of upstream passing/rushing/receiving 2pt conversions.
    pub two_point_conversions: u32,
    pub special_teams_tds: u32,
    /// Offensive fumble-recovery TDs (recovering a fumble and scoring) —
    /// FanDuel's FU/TD.
    pub fumble_recovery_tds: u32,
    pub fg_made_0_19: u32,
    pub fg_made_20_29: u32,
    pub fg_made_30_39: u32,
    pub fg_made_40_49: u32,
    pub fg_made_50_59: u32,
    pub fg_made_60_plus: u32,
    pub fg_missed: u32,
    pub pat_made: u32,
    pub pat_missed: u32,
    /// Upstream scores offense only — kickers are 0 here. Score kicking from
    /// the fg_*/pat_* columns.
    pub fantasy_points: f64,
    pub fantasy_points_ppr: f64,
}

/// One team's per-week D/ST line, aggregated from play-by-play. Every field is
/// a FanDuel D/ST scoring input. `touchdowns` collapses defensive, return, and
/// special-teams TDs (all score +6). `points_allowed` is the opponent's
/// offensive points (FanDuel's points-allowed), not the scoreboard total.
#[derive(Debug, Clone, PartialEq)]
pub struct TeamWeekStats {
    pub season: Season,
    pub week: Week,
    pub season_type: SeasonType,
    pub team: TeamAbbr,
    pub opponent: TeamAbbr,
    pub gsis_game_id: String,
    /// Whole team sacks (one per sack play — includes uncredited strip-sacks).
    pub sacks: u32,
    pub interceptions: u32,
    pub fumble_recoveries: u32,
    pub safeties: u32,
    /// Non-offensive TDs: defensive, return, and special-teams (+6 each).
    pub touchdowns: u32,
    pub blocked_kicks: u32,
    /// Defensive extra-point and two-point returns (+2 each).
    pub conversion_returns: u32,
    /// Opponent's offensive points — FanDuel's D/ST points-allowed.
    pub points_allowed: i32,
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

#[cfg(test)]
mod kickoff_eastern_tests {
    use super::*;
    use time::Weekday;
    use time::macros::datetime;

    fn game(kickoff: Option<time::OffsetDateTime>) -> Game {
        Game {
            gsis_game_id: "x".into(),
            season: Season(2025),
            week: Week(1),
            season_type: SeasonType::Reg,
            kickoff,
            home_team: TeamAbbr("LA".into()),
            away_team: TeamAbbr("SF".into()),
            home_score: None,
            away_score: None,
        }
    }

    #[test]
    fn converts_utc_to_eastern_edt() {
        // 2025-09-07 17:00 UTC == 13:00 EDT (-4), a Sunday.
        let et = game(Some(datetime!(2025-09-07 17:00 UTC)))
            .kickoff_eastern()
            .unwrap();
        assert_eq!(et.weekday(), Weekday::Sunday);
        assert_eq!(et.time(), time::macros::time!(13:00));
    }

    #[test]
    fn converts_utc_to_eastern_est() {
        // 2026-01-10 21:30 UTC == 16:30 EST (-5), a Saturday.
        let et = game(Some(datetime!(2026-01-10 21:30 UTC)))
            .kickoff_eastern()
            .unwrap();
        assert_eq!(et.weekday(), Weekday::Saturday);
        assert_eq!(et.time(), time::macros::time!(16:30));
    }

    #[test]
    fn none_when_kickoff_missing() {
        assert!(game(None).kickoff_eastern().is_none());
    }
}
