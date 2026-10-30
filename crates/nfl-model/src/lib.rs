//! Shared NFL records, live-provider contracts, and Eastern-time helpers.

pub mod eastern;
pub mod live;
mod model;
mod models;

pub use eastern::{eastern_offset, nfl_week_monday, to_eastern};
pub use live::{
    EspnPlayerId, InjuryDesignation, InjuryEntry, InjuryProvider, InjuryReport, LiveGame,
    LiveGamePhase, LiveGameSnapshot, LivePlayerStats, LiveProviderError, LiveScoreProvider,
    LiveScoreboard, LiveScoreboardGame, LiveSlate, LiveTeamStats, ProviderOutcome,
    ProviderResponse, RawBody,
};
pub use model::{
    Game, Player, PlayerWeekStats, Season, SeasonType, TeamWeekStats, Week, WeeklyRosterEntry,
};

pub use models::{DfsPosition, DfsPositionSlotType, NflPlayerId, SeasonWeek, TeamAbbr};
