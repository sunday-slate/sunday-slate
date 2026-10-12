use std::{collections::HashMap, str::FromStr};

use nfl_model::{DfsPosition, SeasonWeek, TeamAbbr};
use sqlx::{SqlitePool, sqlite::SqlitePoolOptions};

use crate::{FanduelDataError, Game, Matchup, Player, SalariedPlayer};

#[derive(Clone)]
pub(crate) struct Store {
    pool: SqlitePool,
}

impl Store {
    pub(crate) async fn connect(database_url: &str) -> Result<Self, FanduelDataError> {
        let options = sqlx::sqlite::SqliteConnectOptions::from_str(database_url)?
            .create_if_missing(true)
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await?;
        Self::from_pool(pool).await
    }

    pub(crate) async fn in_memory() -> Result<Self, FanduelDataError> {
        let options =
            sqlx::sqlite::SqliteConnectOptions::from_str("sqlite::memory:")?.foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await?;
        Self::from_pool(pool).await
    }

    async fn from_pool(pool: SqlitePool) -> Result<Self, FanduelDataError> {
        sqlx::migrate!("./migrations").run(&pool).await?;
        Ok(Self { pool })
    }

    pub(crate) async fn salaried_players_for(
        &self,
        season_week: SeasonWeek,
    ) -> Result<Vec<SalariedPlayer>, FanduelDataError> {
        let rows = sqlx::query!(
            r#"SELECT
                   p.id AS "player_id?",
                   p.first_name AS "first_name?",
                   p.last_name AS "last_name?",
                   g.season_week,
                   g.away_team AS "away_team: TeamAbbr",
                   g.home_team AS "home_team: TeamAbbr",
                   s.salary,
                   s.position AS "position: DfsPosition",
                   s.team AS "team: TeamAbbr"
               FROM salaries s
               JOIN games g ON g.id = s.game_id
               LEFT JOIN players p ON p.id = s.player_id
               WHERE g.season_week = ?
               ORDER BY g.away_team, g.home_team, s.position, s.team, p.id"#,
            i64::from(season_week.encode()),
        )
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter()
            .map(|row| {
                let position = row.position;
                
                let player = match position {
                    DfsPosition::Dst => None,
                    _ => Some(Player {
                        id: row.player_id.unwrap() as u32,
                        first_name: row.first_name.unwrap(),
                        last_name: row.last_name.unwrap()
                    })
                };

                Ok(SalariedPlayer {
                    player,
                    game: Game {
                        season_week: season_week.clone(),
                        matchup: Matchup {
                            away: row.away_team,
                            home: row.home_team,
                        },
                    },
                    salary: row.salary as u32,
                    position,
                    team: row.team,
                })
            })
            .collect()
    }

    pub(crate) async fn save_salaried_players(
        &self,
        salaries: &[SalariedPlayer],
    ) -> Result<(), FanduelDataError> {
        let mut transaction = self.pool.begin().await?;
        let mut game_ids = HashMap::new();

        for salary in salaries {
            if (salary.position == DfsPosition::Dst) != salary.player.is_none() {
                return Err(FanduelDataError::InvalidRow(
                    "DST salaries require no player; other positions require a player".to_owned(),
                ));
            }
            let game = &salary.game;
            let season_week = game.season_week.encode();
            let game_key = (season_week, game.matchup.away, game.matchup.home);
            let game_id = match game_ids.get(&game_key) {
                Some(id) => *id,
                None => {
                    let id = sqlx::query_scalar!(
                        "INSERT INTO games (season_week, away_team, home_team) VALUES (?, ?, ?) ON CONFLICT (season_week, away_team, home_team) DO UPDATE SET id = games.id RETURNING id",
                        i64::from(season_week),
                        game.matchup.away.as_str(),
                        game.matchup.home.as_str(),
                    )
                    .fetch_one(&mut *transaction)
                    .await?;
                    game_ids.insert(game_key, id);
                    id
                }
            };

            if let Some(player) = &salary.player {
                sqlx::query!(
                    "INSERT INTO players (id, first_name, last_name) VALUES (?, ?, ?) ON CONFLICT (id) DO UPDATE SET first_name = excluded.first_name, last_name = excluded.last_name",
                    i64::from(player.id),
                    &player.first_name,
                    &player.last_name,
                )
                .execute(&mut *transaction)
                .await?;

                sqlx::query!(
                    "INSERT INTO salaries (player_id, game_id, salary, position, team) VALUES (?, ?, ?, ?, ?) ON CONFLICT (game_id, player_id) WHERE position <> 'DST' DO UPDATE SET salary = excluded.salary, position = excluded.position, team = excluded.team",
                    i64::from(player.id),
                    game_id,
                    i64::from(salary.salary),
                    salary.position,
                    salary.team.as_str(),
                )
                .execute(&mut *transaction)
                .await?;
            } else {
                if salary.position != DfsPosition::Dst {
                    return Err(FanduelDataError::InvalidRow(
                        "player is required unless position is DST".to_owned(),
                    ));
                }
                sqlx::query!(
                    "INSERT INTO salaries (player_id, game_id, salary, position, team) VALUES (NULL, ?, ?, 'DST', ?) ON CONFLICT (game_id, team) WHERE position = 'DST' DO UPDATE SET salary = excluded.salary",
                    game_id,
                    i64::from(salary.salary),
                    salary.team.as_str(),
                )
                .execute(&mut *transaction)
                .await?;
            }
        }

        transaction.commit().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Game, Matchup, Player};
    use nfl_model::{SeasonWeek, TeamAbbr};

    fn salary(
        position: DfsPosition,
        player: Option<Player>,
        team: TeamAbbr,
        amount: u32,
    ) -> SalariedPlayer {
        SalariedPlayer {
            player,
            game: Game {
                season_week: SeasonWeek::reg(1),
                matchup: Matchup {
                    away: TeamAbbr::BUF,
                    home: TeamAbbr::NYJ,
                },
            },
            salary: amount,
            position,
            team,
        }
    }

    #[tokio::test]
    async fn saves_and_updates_player_and_dst_salaries_idempotently() {
        let store = Store::in_memory().await.unwrap();
        let players = vec![
            salary(
                DfsPosition::Qb,
                Some(Player {
                    id: 62239,
                    first_name: "Josh".into(),
                    last_name: "Allen".into(),
                }),
                TeamAbbr::BUF,
                8200,
            ),
            salary(DfsPosition::Dst, None, TeamAbbr::NYJ, 3500),
        ];

        store.save_salaried_players(&players).await.unwrap();
        let mut updated = players.clone();
        updated[0].salary = 8400;
        updated[1].salary = 3700;
        store.save_salaried_players(&updated).await.unwrap();

        let rows: Vec<(Option<i64>, i64, String)> =
            sqlx::query_as("SELECT player_id, salary, position FROM salaries ORDER BY salary")
                .fetch_all(&store.pool)
                .await
                .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0], (None, 3700, "DST".to_owned()));
        assert_eq!(rows[1], (Some(62239), 8400, "QB".to_owned()));

        let games: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM games")
            .fetch_one(&store.pool)
            .await
            .unwrap();
        let players: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM players")
            .fetch_one(&store.pool)
            .await
            .unwrap();
        assert_eq!(games, 1);
        assert_eq!(players, 1);

        let fetched = store
            .salaried_players_for(SeasonWeek::reg(1))
            .await
            .unwrap();
        assert_eq!(fetched.len(), 2);
        assert!(fetched.iter().any(|salary| {
            salary
                .player
                .as_ref()
                .is_some_and(|player| player.id == 62239)
                && salary.salary == 8400
                && salary.position == DfsPosition::Qb
        }));
        assert!(fetched.iter().any(|salary| {
            salary.player.is_none()
                && salary.team == TeamAbbr::NYJ
                && salary.salary == 3700
                && salary.position == DfsPosition::Dst
        }));
    }
}
