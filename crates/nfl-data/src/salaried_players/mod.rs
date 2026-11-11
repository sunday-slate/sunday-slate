mod salaried_player;
mod service;

pub use salaried_player::{SalariedPlayer, ScoredStats, WeeklyStats};
pub(crate) use service::{SalariedPlayersBySlot, build_salaried_players};
