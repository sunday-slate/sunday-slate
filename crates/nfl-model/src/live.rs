use std::{collections::BTreeMap, fmt, future::Future};

use serde::{Deserialize, Serialize};
use time::{Date, OffsetDateTime};

use crate::{Season, SeasonType, TeamAbbr, Week};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EspnPlayerId(pub String);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LiveSlate {
    pub date: Date,
    pub games: Vec<LiveGame>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LiveGame {
    pub gsis_game_id: String,
    pub provider_game_id: String,
    pub season: Season,
    pub week: Week,
    pub season_type: SeasonType,
    pub kickoff: Option<OffsetDateTime>,
    pub home_team: TeamAbbr,
    pub away_team: TeamAbbr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LiveGamePhase {
    Scheduled,
    InProgress,
    Final,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LiveScoreboard {
    pub games: Vec<LiveScoreboardGame>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LiveScoreboardGame {
    pub provider_game_id: String,
    pub phase: LiveGamePhase,
    pub period: Option<String>,
    pub clock: Option<String>,
    pub home_team: TeamAbbr,
    pub away_team: TeamAbbr,
    pub home_score: Option<i32>,
    pub away_score: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LiveGameSnapshot {
    pub game: LiveGame,
    pub observed_at: OffsetDateTime,
    pub phase: LiveGamePhase,
    pub period: Option<String>,
    pub clock: Option<String>,
    pub home_score: Option<i32>,
    pub away_score: Option<i32>,
    pub players: Option<Vec<LivePlayerStats>>,
    pub defenses: Option<Vec<LiveTeamStats>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LivePlayerStats {
    pub espn_id: Option<EspnPlayerId>,
    pub name: Option<String>,
    pub team: Option<TeamAbbr>,
    pub position: Option<String>,
    pub opponent: Option<TeamAbbr>,
    pub completions: u32,
    pub attempts: u32,
    pub passing_tds: u32,
    pub passing_interceptions: u32,
    pub rushing_attempts: u32,
    pub rushing_tds: u32,
    pub targets: u32,
    pub receptions: u32,
    pub receiving_tds: u32,
    pub fumbles_lost: u32,
    pub two_point_conversions: u32,
    pub special_teams_tds: u32,
    pub fumble_recovery_tds: u32,
    pub passing_yards: i32,
    pub rushing_yards: i32,
    pub receiving_yards: i32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LiveTeamStats {
    pub team: TeamAbbr,
    pub opponent: TeamAbbr,
    pub sacks: u32,
    pub interceptions: u32,
    pub fumble_recoveries: u32,
    pub safeties: u32,
    pub touchdowns: u32,
    pub blocked_kicks: u32,
    pub conversion_returns: u32,
    pub points_allowed: i32,
    pub dst_present: bool,
    pub team_stats_present: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum RawBody {
    Json(serde_json::Value),
    Text(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
pub enum LiveProviderError {
    #[error("transport error: {message}")]
    Transport { message: String },
    #[error("provider returned HTTP {status}: {message}")]
    Http { status: u16, message: String },
    #[error("provider JSON error: {message}")]
    Json { message: String },
    #[error("provider normalization error: {message}")]
    Normalization { message: String },
    #[error("provider identity conflict: {message}")]
    IdentityConflict { message: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ProviderOutcome<T> {
    Value(T),
    Pregame { message: String },
    Error(LiveProviderError),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderResponse<T> {
    pub requested_at: OffsetDateTime,
    pub received_at: OffsetDateTime,
    pub elapsed_ms: u64,
    pub http_status: Option<u16>,
    pub headers: BTreeMap<String, String>,
    pub raw_body: Option<RawBody>,
    pub outcome: ProviderOutcome<T>,
}

pub trait LiveScoreProvider: Send + Sync {
    fn scoreboard(
        &self,
        date: Date,
    ) -> impl Future<Output = ProviderResponse<LiveScoreboard>> + Send;

    fn box_score(
        &self,
        game: &LiveGame,
        play_by_play: bool,
    ) -> impl Future<Output = ProviderResponse<LiveGameSnapshot>> + Send;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(rename_all = "snake_case")]
pub enum InjuryDesignation {
    Questionable,
    Doubtful,
    Out,
    InjuredReserve,
}

impl fmt::Display for InjuryDesignation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Questionable => "Q",
            Self::Doubtful => "D",
            Self::Out => "O",
            Self::InjuredReserve => "IR",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InjuryEntry {
    pub espn_id: Option<EspnPlayerId>,
    pub name: String,
    pub team: Option<TeamAbbr>,
    pub position: Option<String>,
    pub designation: InjuryDesignation,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InjuryReport {
    /// The `injDate` the provider echoed, for diagnostics only.
    pub report_date: Option<Date>,
    pub entries: Vec<InjuryEntry>,
}

pub trait InjuryProvider: Send + Sync {
    fn current_injuries(&self) -> impl Future<Output = ProviderResponse<InjuryReport>> + Send;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn designation_serde_and_display_use_short_codes() {
        let round = |json: &str, value: InjuryDesignation, code: &str| {
            assert_eq!(
                serde_json::from_str::<InjuryDesignation>(json).unwrap(),
                value
            );
            assert_eq!(serde_json::to_string(&value).unwrap(), json);
            assert_eq!(value.to_string(), code);
        };
        round("\"questionable\"", InjuryDesignation::Questionable, "Q");
        round("\"doubtful\"", InjuryDesignation::Doubtful, "D");
        round("\"out\"", InjuryDesignation::Out, "O");
        round(
            "\"injured_reserve\"",
            InjuryDesignation::InjuredReserve,
            "IR",
        );
    }

    #[test]
    fn injury_entries_and_reports_roundtrip() {
        let report = InjuryReport {
            report_date: Some(time::macros::date!(2026 - 09 - 22)),
            entries: vec![InjuryEntry {
                espn_id: Some(EspnPlayerId("4044138".into())),
                name: "Matt Hennessy".into(),
                team: Some(TeamAbbr("DAL".into())),
                position: Some("C".into()),
                designation: InjuryDesignation::InjuredReserve,
            }],
        };
        let text = serde_json::to_string(&report).unwrap();
        assert_eq!(serde_json::from_str::<InjuryReport>(&text).unwrap(), report);
        assert!(text.contains(r#""designation":"injured_reserve""#));
        assert!(text.contains("2026"));
    }
}
