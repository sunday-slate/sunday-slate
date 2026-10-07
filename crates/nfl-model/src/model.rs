use time::OffsetDateTime;

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
    pub headshot_url: Option<String>,
}

/// One player's place on a team's roster in a single week. This survives
/// mid-season trades: a player appears under each team for the weeks he was
/// actually there.
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
