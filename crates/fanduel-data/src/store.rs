use crate::{FanduelDataError, Game, Matchup, Player, SalariedPlayer};
use nfl_model::{DfsPosition, SeasonWeek, TeamAbbr};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{QueryBuilder, Sqlite, SqlitePool};
use std::collections::BTreeMap;
use std::{collections::HashMap, str::FromStr};

#[derive(Clone)]
pub(crate) struct Store {
    pool: SqlitePool,
}

impl Store {
    // Connect to an existing store. Database should exist and be fully migrated.
    pub(crate) async fn connect(database_url: &str) -> Result<Self, FanduelDataError> {
        let options = SqliteConnectOptions::from_str(database_url)?.foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await?;

        Ok(Self { pool })
    }

    // Create an empty in-memory store with all migrations applied
    pub(crate) async fn in_memory() -> Result<Self, FanduelDataError> {
        let store = Self::connect("sqlite::memory:").await?;
        sqlx::migrate!("./migrations").run(&store.pool).await?;
        Ok(store)
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
            season_week,
        )
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter()
            .map(|row| {
                let position = row.position;

                let player = match position {
                    DfsPosition::Dst => None,
                    _ => Some(Player {
                        // safe unwraps due to DB constraint when non-DST
                        id: row.player_id.unwrap() as u32,
                        first_name: row.first_name.unwrap(),
                        last_name: row.last_name.unwrap(),
                    }),
                };

                Ok(SalariedPlayer {
                    player,
                    game: Game {
                        season_week,
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
            let game_key = (game.season_week, game.matchup.away, game.matchup.home);
            let game_id = match game_ids.get(&game_key) {
                Some(id) => *id,
                None => {
                    let id = sqlx::query_scalar!(
                        "INSERT INTO games (season_week, away_team, home_team) VALUES (?, ?, ?) ON CONFLICT (season_week, away_team, home_team) DO UPDATE SET id = games.id RETURNING id",
                        game.season_week,
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

    pub(crate) async fn salary_counts_by_week(
        &self,
    ) -> Result<BTreeMap<SeasonWeek, usize>, FanduelDataError> {
        let rows = sqlx::query!(
            r#"
           SELECT
               games.season_week AS "season_week: SeasonWeek",
               COUNT(*) AS "salary_count: u32"
           FROM salaries
           JOIN games ON games.id = salaries.game_id
           GROUP BY games.season_week
           "#
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(|row| (row.season_week, row.salary_count as usize))
            .collect())
    }

    pub(crate) async fn unmatched_counts_by_week(
        &self,
        matched_ids: &[u32],
    ) -> Result<BTreeMap<SeasonWeek, usize>, FanduelDataError> {
        let mut query = QueryBuilder::<Sqlite>::new(
            r#"
            SELECT
                games.season_week,
                COUNT(*) AS salary_count
            FROM salaries
            JOIN games ON games.id = salaries.game_id
            WHERE salaries.player_id IS NOT NULL
            "#,
        );

        // DST rows are always matched; an empty list leaves every player unmatched.
        if !matched_ids.is_empty() {
            query.push(" AND salaries.player_id NOT IN (");

            let mut separated = query.separated(", ");
            for &id in matched_ids {
                separated.push_bind(id);
            }
            separated.push_unseparated(")");
        }

        query.push(" GROUP BY games.season_week");

        let rows = query
            .build_query_as::<(SeasonWeek, u32)>()
            .fetch_all(&self.pool)
            .await?;

        Ok(rows.into_iter().map(|(w, c)| (w, c as usize)).collect())
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
    async fn salary_counts_by_week_groups_complete_season_weeks() {
        let store = Store::in_memory().await.unwrap();
        let regular = salary(
            DfsPosition::Qb,
            Some(Player {
                id: 62239,
                first_name: "Josh".into(),
                last_name: "Allen".into(),
            }),
            TeamAbbr::BUF,
            8200,
        );
        let mut regular_dst = salary(DfsPosition::Dst, None, TeamAbbr::NYJ, 3500);
        let mut postseason = regular.clone();
        postseason.game.season_week = SeasonWeek::post(1);
        postseason.salary = 7800;
        regular_dst.game.season_week = SeasonWeek::reg(1);
        store
            .save_salaried_players(&[regular, regular_dst, postseason])
            .await
            .unwrap();

        assert_eq!(
            store.salary_counts_by_week().await.unwrap(),
            vec![(SeasonWeek::post(1), 1), (SeasonWeek::reg(1), 2)]
                .into_iter()
                .collect()
        );
    }

    #[tokio::test]
    async fn unmatched_counts_exclude_dst_and_matched_players() {
        let store = Store::in_memory().await.unwrap();
        assert!(
            store
                .unmatched_counts_by_week(&[])
                .await
                .unwrap()
                .is_empty()
        );

        store
            .save_salaried_players(&[
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
                salary(
                    DfsPosition::Wr,
                    Some(Player {
                        id: 99999,
                        first_name: "Mystery".into(),
                        last_name: "Receiver".into(),
                    }),
                    TeamAbbr::NYJ,
                    5000,
                ),
                salary(DfsPosition::Dst, None, TeamAbbr::BUF, 3500),
            ])
            .await
            .unwrap();

        assert_eq!(
            store.unmatched_counts_by_week(&[]).await.unwrap(),
            BTreeMap::from([(SeasonWeek::reg(1), 2)])
        );
        assert_eq!(
            store.unmatched_counts_by_week(&[62239]).await.unwrap(),
            BTreeMap::from([(SeasonWeek::reg(1), 1)])
        );
        // Weeks with no unmatched players are omitted rather than returned with zero.
        assert!(
            store
                .unmatched_counts_by_week(&[62239, 99999])
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn failed_save_rolls_back_the_batch_and_preserves_existing_rows() {
        for invalid in [
            salary(DfsPosition::Qb, None, TeamAbbr::BUF, 8200),
            salary(DfsPosition::Dst, None, TeamAbbr::NYJ, 0),
        ] {
            let store = Store::in_memory().await.unwrap();
            let existing = salary(DfsPosition::Dst, None, TeamAbbr::BUF, 3500);
            store
                .save_salaried_players(std::slice::from_ref(&existing))
                .await
                .unwrap();
            let mut new_player = salary(
                DfsPosition::Qb,
                Some(Player {
                    id: 62239,
                    first_name: "Josh".into(),
                    last_name: "Allen".into(),
                }),
                TeamAbbr::BUF,
                8200,
            );
            new_player.game.season_week = SeasonWeek::reg(2);

            assert!(
                store
                    .save_salaried_players(&[new_player, invalid])
                    .await
                    .is_err()
            );
            assert_eq!(
                store
                    .salaried_players_for(SeasonWeek::reg(1))
                    .await
                    .unwrap(),
                vec![existing]
            );
            let counts: (i64, i64, i64) = sqlx::query_as(
                "SELECT (SELECT COUNT(*) FROM games),
                        (SELECT COUNT(*) FROM players),
                        (SELECT COUNT(*) FROM salaries)",
            )
            .fetch_one(&store.pool)
            .await
            .unwrap();
            assert_eq!(counts, (1, 0, 1));
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
            salary(DfsPosition::Dst, None, TeamAbbr::BUF, 3300),
            salary(DfsPosition::Dst, None, TeamAbbr::NYJ, 3500),
        ];

        store.save_salaried_players(&players).await.unwrap();
        store.save_salaried_players(&players).await.unwrap();
        let mut updated = players.clone();
        updated[0].salary = 8400;
        updated[0].position = DfsPosition::Rb;
        updated[0].team = TeamAbbr::NYJ;
        let player = updated[0].player.as_mut().unwrap();
        player.first_name = "Updated".into();
        player.last_name = "Player".into();
        updated[1].salary = 3600;
        updated[2].salary = 3700;
        store.save_salaried_players(&updated).await.unwrap();
        store.save_salaried_players(&updated).await.unwrap();

        let rows: Vec<(Option<i64>, i64, String, String)> = sqlx::query_as(
            "SELECT player_id, salary, position, team FROM salaries ORDER BY salary",
        )
        .fetch_all(&store.pool)
        .await
        .unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0], (None, 3600, "DST".to_owned(), "BUF".to_owned()));
        assert_eq!(rows[1], (None, 3700, "DST".to_owned(), "NYJ".to_owned()));
        assert_eq!(
            rows[2],
            (Some(62239), 8400, "RB".to_owned(), "NYJ".to_owned())
        );

        let names: (String, String) =
            sqlx::query_as("SELECT first_name, last_name FROM players WHERE id = 62239")
                .fetch_one(&store.pool)
                .await
                .unwrap();
        assert_eq!(names, ("Updated".to_owned(), "Player".to_owned()));

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
    }

    #[tokio::test]
    async fn salaried_players_for_returns_player_and_dst_rows() {
        let store = Store::in_memory().await.unwrap();
        let salaries = vec![
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
        store.save_salaried_players(&salaries).await.unwrap();

        let fetched = store
            .salaried_players_for(SeasonWeek::reg(1))
            .await
            .unwrap();
        assert_eq!(fetched.len(), 2);
        assert!(fetched.iter().any(|salary| {
            salary.player.as_ref().is_some_and(|player| {
                player.id == 62239 && player.first_name == "Josh" && player.last_name == "Allen"
            }) && salary.salary == 8200
                && salary.position == DfsPosition::Qb
                && salary.team == TeamAbbr::BUF
                && salary.game.matchup.away == TeamAbbr::BUF
                && salary.game.matchup.home == TeamAbbr::NYJ
        }));
        assert!(fetched.iter().any(|salary| {
            salary.player.is_none()
                && salary.salary == 3500
                && salary.position == DfsPosition::Dst
                && salary.team == TeamAbbr::NYJ
        }));
    }

    #[tokio::test]
    async fn salaried_players_for_filters_by_season_week_and_returns_empty_when_missing() {
        let store = Store::in_memory().await.unwrap();
        let week_one = salary(
            DfsPosition::Qb,
            Some(Player {
                id: 62239,
                first_name: "Josh".into(),
                last_name: "Allen".into(),
            }),
            TeamAbbr::BUF,
            8200,
        );
        let mut week_two = week_one.clone();
        week_two.game.season_week = SeasonWeek::reg(2);
        week_two.salary = 8100;
        store
            .save_salaried_players(&[week_one.clone(), week_two])
            .await
            .unwrap();

        let fetched = store
            .salaried_players_for(SeasonWeek::reg(1))
            .await
            .unwrap();
        assert_eq!(fetched, vec![week_one]);
        assert!(
            store
                .salaried_players_for(SeasonWeek::reg(3))
                .await
                .unwrap()
                .is_empty()
        );
    }
}
