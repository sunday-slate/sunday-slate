//! Shared NFL records, live-provider contracts, and Eastern-time helpers.

pub mod eastern;
pub mod live;
mod model;

pub use eastern::{eastern_offset, to_eastern};
pub use live::{
    EspnPlayerId, InjuryDesignation, InjuryEntry, InjuryProvider, InjuryReport, LiveGame,
    LiveGamePhase, LiveGameSnapshot, LivePlayerStats, LiveProviderError, LiveScoreProvider,
    LiveScoreboard, LiveScoreboardGame, LiveSlate, LiveTeamStats, ProviderOutcome,
    ProviderResponse, RawBody,
};
pub use model::{
    DfsPosition, Game, Player, PlayerWeekStats, Season, SeasonType, TeamAbbr, TeamWeekStats, Week,
    WeeklyRosterEntry,
};
