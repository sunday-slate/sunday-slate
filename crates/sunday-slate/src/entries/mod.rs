pub mod handlers;
pub mod model;
pub mod service;
pub mod store;

pub use model::{
    ContestEntry, EntryDetails, EntryId, EntrySlot, EntryTeam, FantasyTeamEntries,
    FantasyTeamEntry, Lineup, LineupError, NflPlayerId, RosterSlot, SlotValue,
};
pub use store::EntriesError;

use crate::AppState;
use axum::{Router, routing::get};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/entries/{id}", get(handlers::show))
        .route("/entries/{id}/events", get(handlers::events))
}
