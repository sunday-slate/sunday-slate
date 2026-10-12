mod csv;
mod error;
mod models;
mod store;

pub use error::FanduelDataError;
pub use models::{Game, Matchup, Player, SalariedPlayer};

use nfl_model::SeasonWeek;

#[derive(Clone)]
pub struct FanduelData {
    store: store::Store,
}

impl FanduelData {
    pub async fn connect(database_url: &str) -> Result<Self, FanduelDataError> {
        Ok(Self {
            store: store::Store::connect(database_url).await?,
        })
    }

    pub async fn in_memory() -> Result<Self, FanduelDataError> {
        Ok(Self {
            store: store::Store::in_memory().await?,
        })
    }

    pub async fn import_csv(
        &self,
        season_week: SeasonWeek,
        csv_data: &[u8],
    ) -> Result<Vec<SalariedPlayer>, FanduelDataError> {
        let salaries = csv::parse_csv(season_week, csv_data)?;
        self.save_salaried_players(&salaries).await?;
        Ok(salaries)
    }

    pub async fn save_salaried_players(
        &self,
        salaries: &[SalariedPlayer],
    ) -> Result<(), FanduelDataError> {
        self.store.save_salaried_players(salaries).await
    }

    pub async fn salaried_players_for(
        &self,
        season_week: SeasonWeek,
    ) -> Result<Vec<SalariedPlayer>, FanduelDataError> {
        self.store.salaried_players_for(season_week).await
    }
}
