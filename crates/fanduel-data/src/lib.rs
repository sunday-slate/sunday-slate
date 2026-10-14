mod csv;
mod error;
mod models;
mod store;

pub use error::FanduelDataError;
pub use models::{Game, Matchup, Player, SalariedPlayer};

use csv::parse_csv;
use nfl_model::SeasonWeek;
use store::Store;

#[derive(Clone)]
pub struct FanduelData {
    store: Store,
}

impl FanduelData {
    /// Connect to an existing store. Database should exist and be fully migrated.
    pub async fn connect(database_url: &str) -> Result<Self, FanduelDataError> {
        Ok(Self {
            store: Store::connect(database_url).await?,
        })
    }

    /// Create an empty in-memory store with all migrations applied
    pub async fn in_memory() -> Result<Self, FanduelDataError> {
        Ok(Self {
            store: Store::in_memory().await?,
        })
    }

    /// Take a fanduel csv file and parse to SalariedPlayers. No data is saved.
    /// A SeasonWeek is required because fanduel's CSV does not contain the NFL Week represented
    /// by the file.
    pub fn parse_fanduel_csv(
        &self,
        season_week: SeasonWeek,
        csv_data: &[u8],
    ) -> Result<Vec<SalariedPlayer>, FanduelDataError> {
        parse_csv(season_week, csv_data)
    }

    /// Store SalariedPlayers in the DB. Usually generated from parse_fanduel_csv.
    pub async fn save_salaried_players(
        &self,
        salaries: &[SalariedPlayer],
    ) -> Result<(), FanduelDataError> {
        self.store.save_salaried_players(salaries).await
    }

    /// Count of stored player salaries by week
    pub async fn salary_counts_by_week(&self) -> Result<Vec<(SeasonWeek, i64)>, FanduelDataError> {
        self.store.salary_counts_by_week().await
    }

    /// All salaried players for a given week
    pub async fn salaried_players_for(
        &self,
        season_week: SeasonWeek,
    ) -> Result<Vec<SalariedPlayer>, FanduelDataError> {
        self.store.salaried_players_for(season_week).await
    }
}
