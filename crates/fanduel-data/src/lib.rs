mod csv;
mod error;
mod models;

pub use error::FanduelDataError;
pub use models::{Game, Matchup, Player, SalariedPlayer};

use std::collections::BTreeMap;

use nfl_model::{DfsPosition, SeasonWeek, TeamAbbr};
use sqlx::{SqlitePool, sqlite::SqlitePoolOptions};

#[derive(Clone)]
pub struct FanduelData {
    pool: SqlitePool,
}

impl FanduelData {
    pub async fn connect(database_url: &str) -> Result<Self, FanduelDataError> {
        let options = sqlx::sqlite::SqliteConnectOptions::from_str(database_url)?
            .create_if_missing(true)
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await?;
        Self::from_pool(pool).await
    }

    pub async fn in_memory() -> Result<Self, FanduelDataError> {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await?;
        Self::from_pool(pool).await
    }

    async fn from_pool(pool: SqlitePool) -> Result<Self, FanduelDataError> {
        sqlx::migrate!("./migrations").run(&pool).await?;
        Ok(Self { pool })
    }

    pub async fn import_csv(
        &self,
        season_week: SeasonWeek,
        csv_data: &[u8],
    ) -> Result<Vec<SalariedPlayer>, FanduelDataError> {
        csv::parse(csv_data)?.into_iter().map(|row| {
            let position = row.position().ok_or_else(|| {
                FanduelDataError::InvalidRow(format!("unknown position {:?}", row.position))
            })?;

            let salary = row
                .salary
                .parse::<u32>()
                .ok()
                .filter(|salary| *salary > 0)
                .ok_or_else(|| {
                    FanduelDataError::InvalidRow(format!("invalid salary {:?}", row.salary))
                })?;

            let game = Game {
                matchup: row.game.parse()?,
                season_week: season_week.clone()
            };

            let team = row
                .team
                .parse::<TeamAbbr>()
                .map_err(|error| FanduelDataError::InvalidRow(error.to_string()))?;

            let player = if position == DfsPosition::Dst {
                None
            } else {
                let first_name = row.first_name.trim().to_owned();
                let last_name = row.last_name.trim().to_owned();

                if first_name.is_empty() || last_name.is_empty() {
                    return Err(FanduelDataError::InvalidRow(
                        "player name is missing".to_owned(),
                    ));
                }

                let (_, id) = row.id.split_once('-').ok_or(FanduelDataError::InvalidRow(format!("invalid player id {:?}", row.id)))?;
                let id: u32 = id.parse().map_err(|_| FanduelDataError::InvalidRow(format!("invalid player id {:?}", row.id)))?;

                Some(Player {
                    id,
                    first_name,
                    last_name
                })
            };

            Ok(SalariedPlayer {
                player,
                game,
                salary,
                position,
                team,
            })
        }).collect()

        // let row_count = grouped_rows.values().map(Vec::len).sum();
        // let season_week_value = i64::from(season_week.encode());
        // let mut result = Vec::with_capacity(row_count);
        // let mut transaction = self.pool.begin().await?;
        // for ((away, home), rows) in grouped_rows {
        //     let matchup = Matchup { away, home };
        //     let game_id = sqlx::query_scalar!(
        //         "INSERT INTO games (season_week, away_team, home_team) VALUES (?, ?, ?) ON CONFLICT (season_week, away_team, home_team) DO UPDATE SET id = games.id RETURNING id",
        //         season_week_value,
        //         matchup.away.as_str(),
        //         matchup.home.as_str(),
        //     )
        //     .fetch_one(&mut *transaction)
        //     .await?;
        //
        //     for (row, position, salary, team, player_id) in rows {
        //         let player = if let Some(player_id) = player_id {
        //             sqlx::query!(
        //                 "INSERT INTO players (id, first_name, last_name) VALUES (?, ?, ?) ON CONFLICT (id) DO UPDATE SET first_name = excluded.first_name, last_name = excluded.last_name",
        //                 i64::from(player_id),
        //                 &row.first_name,
        //                 &row.last_name,
        //             )
        //             .execute(&mut *transaction)
        //             .await?;
        //             Some(Player {
        //                 id: player_id,
        //                 first_name: row.first_name,
        //                 last_name: row.last_name,
        //             })
        //         } else {
        //             None
        //         };
        //
        //         sqlx::query!(
        //             "INSERT INTO salaries (player_id, game_id, salary, position, team) VALUES (?, ?, ?, ?, ?)",
        //             player.as_ref().map(|player| i64::from(player.id)),
        //             game_id,
        //             i64::from(salary),
        //             position_value(position),
        //             team.as_str(),
        //         )
        //         .execute(&mut *transaction)
        //         .await?;
        //
        //         result.push(SalariedPlayer {
        //             player,
        //             game: Game {
        //                 season_week: season_week.clone(),
        //                 matchup: matchup.clone(),
        //             },
        //             salary,
        //             position,
        //             team,
        //         });
        //     }
        // }
        //
        // transaction.commit().await?;
        // Ok(result)
    }
}
