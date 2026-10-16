//! FanDuel DFS salary CSV parsing and SQLite storage.
//!
//! This crate is primarily used by `nfl-data` to import FanDuel salary CSV files. It parses
//! CSV rows into [`SalariedPlayer`] records, then stores and retrieves them by NFL season week.
//!
//! FanDuel salary CSV files do not include the NFL week, so parsing requires the caller to
//! provide a [`SeasonWeek`]. Parsing is separate from persistence, allowing the
//! returned records to be inspected before saving.
//!
//! # Importing salaries
//!
//! Connect, creating the database and applying this crate's migrations as
//! needed, parse the export, and save the resulting records.
//!
//! ```no_run
//! use fanduel_data::FanduelData;
//! use nfl_model::SeasonWeek;
//!
//! # async fn import(database_url: &str, csv_bytes: &[u8]) -> Result<(), Box<dyn std::error::Error>> {
//! let data = FanduelData::connect(database_url).await?;
//! let week = SeasonWeek::reg(1);
//! let salaries = data.parse_fanduel_csv(week, csv_bytes)?;
//! data.save_salaried_players(&salaries).await?;
//!
//! let stored = data.salaried_players_for(week).await?;
//! println!("Stored {} salary entries", stored.len());
//! # Ok(())
//! # }
//! ```
//!
//! [DFS positions](nfl_model::DfsPosition) and [team abbreviations](nfl_model::TeamAbbr) use
//! shared types from `nfl-model`. [`DST`](nfl_model::DfsPosition::Dst) entries represent team
//! defenses and have no [`Player`]; entries at other positions contain a player.
//! [`weekly_unmatched_counts`](FanduelData::weekly_unmatched_counts) counts unmatched player
//! entries only, excluding defenses.

mod csv;
mod error;
mod models;
mod store;

pub use error::FanduelDataError;
pub use models::{Game, Matchup, Player, SalariedPlayer};
use std::collections::BTreeMap;

use csv::parse_csv;
use nfl_model::SeasonWeek;
use store::Store;

/// A handle for parsing and storing FanDuel salary data in SQLite.
///
/// Clone values share the same underlying database connection pool.
#[derive(Clone)]
pub struct FanduelData {
    store: Store,
}

impl FanduelData {
    /// Connect, creating the database and applying this crate's migrations
    /// if needed. Migration applies are idempotent.
    pub async fn connect(database_url: &str) -> Result<Self, FanduelDataError> {
        Ok(Self {
            store: Store::connect(database_url).await?,
        })
    }

    /// Create an empty in-memory database and apply all migrations.
    ///
    /// This constructor is primarily intended for tests.
    pub async fn in_memory() -> Result<Self, FanduelDataError> {
        Ok(Self {
            store: Store::in_memory().await?,
        })
    }

    /// Parse a FanDuel salary CSV into typed records without saving them.
    ///
    /// The season week must be provided because the CSV does not identify the NFL week it
    /// represents. Review or modify the returned records before passing them to
    /// [`save_salaried_players`](Self::save_salaried_players).
    pub fn parse_fanduel_csv(
        &self,
        season_week: SeasonWeek,
        csv_data: &[u8],
    ) -> Result<Vec<SalariedPlayer>, FanduelDataError> {
        parse_csv(season_week, csv_data)
    }

    /// Save salary records to the database, typically after parsing them with
    /// [`parse_fanduel_csv`](Self::parse_fanduel_csv).
    ///
    /// Existing records for the same player and game (or defense team and game) are updated,
    /// making repeated imports safe. The entire slice is saved as one transaction.
    pub async fn save_salaried_players(
        &self,
        salaries: &[SalariedPlayer],
    ) -> Result<(), FanduelDataError> {
        self.store.save_salaried_players(salaries).await
    }

    /// Count all salary entries for each season week present in the database.
    ///
    /// Weeks with no entries are omitted.
    pub async fn weekly_salary_counts(
        &self,
    ) -> Result<BTreeMap<SeasonWeek, usize>, FanduelDataError> {
        self.store.salary_counts_by_week().await
    }

    /// Count unmatched player salary entries by season week.
    ///
    /// `matched_ids` lists player IDs that have been matched elsewhere. Defense (`DST`) entries
    /// are excluded, and weeks with no unmatched players are omitted.
    pub async fn weekly_unmatched_counts(
        &self,
        matched_ids: &[u32],
    ) -> Result<BTreeMap<SeasonWeek, usize>, FanduelDataError> {
        self.store.unmatched_counts_by_week(matched_ids).await
    }

    /// Return all player and defense salary entries for a season week.
    ///
    /// Returns an empty vector when the database has no entries for that week.
    pub async fn salaried_players_for(
        &self,
        season_week: SeasonWeek,
    ) -> Result<Vec<SalariedPlayer>, FanduelDataError> {
        self.store.salaried_players_for(season_week).await
    }
}
