use std::fmt::{Display, Formatter};
use time::OffsetDateTime;

const DEFAULT_SEASON: u16 = 2026;

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

impl Default for Season {
    fn default() -> Self {
        Season(DEFAULT_SEASON)
    }
}

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
    strum::AsRefStr,
    strum::EnumString,
)]
#[allow(non_camel_case_types)]
#[serde(rename_all = "UPPERCASE")]
#[sqlx(type_name = "TEXT", rename_all = "UPPERCASE")]
#[strum(serialize_all = "UPPERCASE", ascii_case_insensitive)]
pub enum TeamAbbr {
    ARI,
    ATL,
    BAL,
    BUF,
    CAR,
    CHI,
    CIN,
    CLE,
    DAL,
    DEN,
    DET,
    GB,
    HOU,
    IND,
    #[serde(alias = "JAC")]
    #[strum(serialize = "JAX", serialize = "JAC")]
    JAX,
    KC,
    LAC,
    #[serde(alias = "LAR")]
    #[strum(serialize = "LA", serialize = "LAR")]
    LA,
    LV,
    MIA,
    MIN,
    NE,
    NO,
    NYG,
    NYJ,
    PHI,
    PIT,
    SEA,
    SF,
    TB,
    TEN,
    WAS,
}

impl TeamAbbr {
    pub fn as_str(&self) -> &str {
        match self {
            Self::JAX => "JAX",
            Self::LA => "LA",
            _ => self.as_ref(),
        }
    }
}

impl Display for TeamAbbr {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Stored as TEXT "REG"/"POST". Upstream playoff game types (WC/DIV/CON/SB)
/// map to Post at ingest.
#[repr(u8)]
#[derive(
    Default,
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    serde::Serialize,
    serde::Deserialize,
    sqlx::Type,
)]
#[serde(rename_all = "UPPERCASE")]
pub enum SeasonType {
    #[default]
    Reg = 0,
    Post = 1,
}

impl From<SeasonType> for u8 {
    fn from(value: SeasonType) -> Self {
        value as u8
    }
}

impl From<SeasonType> for u32 {
    fn from(value: SeasonType) -> Self {
        value as u32
    }
}

impl TryFrom<u8> for SeasonType {
    type Error = ();

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Reg),
            1 => Ok(Self::Post),
            _ => Err(()),
        }
    }
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

#[cfg(test)]
mod team_abbr_tests {
    use super::TeamAbbr;

    #[test]
    fn parses_legacy_abbreviations_to_canonical_variants() {
        assert_eq!("JAC".parse(), Ok(TeamAbbr::JAX));
        assert_eq!("LAR".parse(), Ok(TeamAbbr::LA));
        assert_eq!(TeamAbbr::JAX.as_str(), "JAX");
        assert_eq!(TeamAbbr::LA.as_str(), "LA");
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
            home_team: TeamAbbr::LA,
            away_team: TeamAbbr::SF,
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
