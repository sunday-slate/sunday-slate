mod candidates;
mod handlers;
pub mod matcher;
pub mod schedule;

pub use handlers::router;

use std::collections::{BTreeSet, HashMap};

use nfl_data::{Game, NflData, SalariedPlayer, Season, SeasonType, SeasonWeek, Week};

use crate::Db;
use crate::admin::salary_imports::matcher::{PlayerIndex, RosterIndex};
use crate::admin::salary_imports::schedule::resolve_slate_games;
use crate::nfl_players::store as player_store;
use crate::player_salaries::model::{DfsPosition, NflPlayerSalary};
use crate::player_salaries::store as salary_store;

/// One stored contest week and the source player IDs still needing a crosswalk.
#[derive(Debug, Clone, PartialEq)]
pub struct SalaryWeekOverview {
    pub season_week: SeasonWeek,
    pub salary_count: usize,
    pub unmatched: Vec<SalariedPlayer>,
}

#[derive(Debug, thiserror::Error)]
pub enum SalaryImportError {
    #[error("FanDuel salary import failed: {0}")]
    Fanduel(#[from] nfl_data::FanduelDataError),
    #[error("NFL schedule lookup failed: {0}")]
    Nfl(#[from] nfl_data::NflDataError),
    #[error("salary matchup does not exist in the requested season week")]
    Schedule,
    #[error("Sunday Slate database operation failed: {0}")]
    Database(#[from] sqlx::Error),
}

/// Validate the typed CSV's schedule, persist it, and project every mapped salary.
pub async fn import_salary_csv(
    db: &Db,
    nfl: &NflData,
    season_week: SeasonWeek,
    csv_data: &[u8],
) -> Result<SalaryWeekOverview, SalaryImportError> {
    let source = nfl.parse_fanduel_csv(season_week, csv_data)?;
    let games = nfl.games(season_week.season).await?;
    resolve_slate_games(&games, &season_week, &salary_matchups(&source))
        .map_err(|_| SalaryImportError::Schedule)?;
    nfl.save_fanduel_salaries(&source).await?;
    project_salary_week(db, nfl, season_week, None).await?;
    salary_week_overview(db, nfl, season_week).await
}

pub async fn salary_week_overview(
    db: &Db,
    nfl: &NflData,
    season_week: SeasonWeek,
) -> Result<SalaryWeekOverview, SalaryImportError> {
    let salaries = nfl.salaried_players_for(season_week).await?;
    let salary_count = salaries.len();

    let matched_ids = player_store::mapped_fd_ids(db.reader()).await?;
    let unmatched = salaries
        .into_iter()
        .filter(|salaried_player| match &salaried_player.player {
            Some(player) => !matched_ids.contains(&player.id),
            None => false,
        })
        .collect();

    Ok(SalaryWeekOverview {
        season_week,
        salary_count,
        unmatched,
    })
}

pub async fn resolve_salary_player(
    db: &Db,
    nfl: &NflData,
    season_week: SeasonWeek,
    fd_player_id: u32,
    gsis_player_id: &str,
) -> Result<SalaryWeekOverview, SalaryImportError> {
    let source = nfl.salaried_players_for(season_week).await?;
    if !source.iter().any(|salary| {
        salary
            .player
            .as_ref()
            .is_some_and(|player| player.id == fd_player_id)
    }) {
        return Err(SalaryImportError::Schedule);
    }
    project_salary_week(db, nfl, season_week, Some((fd_player_id, gsis_player_id))).await?;
    salary_week_overview(db, nfl, season_week).await
}

fn salary_matchups(source: &[SalariedPlayer]) -> BTreeSet<(String, String)> {
    source
        .iter()
        .map(|salary| {
            (
                salary.game.matchup.away.as_str().to_owned(),
                salary.game.matchup.home.as_str().to_owned(),
            )
        })
        .collect()
}

async fn project_salary_week(
    db: &Db,
    nfl: &NflData,
    season_week: SeasonWeek,
    resolved_player: Option<(u32, &str)>,
) -> Result<(), SalaryImportError> {
    let source = nfl.salaried_players_for(season_week).await?;
    let matchups = salary_matchups(&source);
    let games = nfl.games(season_week.season).await?;
    let slate_games = resolve_slate_games(&games, &season_week, &matchups)
        .map_err(|_| SalaryImportError::Schedule)?;
    let game_of: HashMap<(String, String), String> = slate_games
        .iter()
        .map(|game| {
            (
                (
                    game.away_team.as_str().to_owned(),
                    game.home_team.as_str().to_owned(),
                ),
                game.gsis_game_id.clone(),
            )
        })
        .collect();
    let players = nfl.players().await?;
    let player_index = PlayerIndex::build(&players);
    let mut roster_entries = Vec::new();
    let mut teams = BTreeSet::new();
    for game in &slate_games {
        teams.insert(game.home_team);
        teams.insert(game.away_team);
    }
    for team in teams {
        roster_entries.extend(
            nfl.weekly_roster(season_week.season, season_week.week, &team)
                .await?,
        );
    }
    let roster_index = RosterIndex::build(&roster_entries);
    let mut mapped = Vec::new();
    for salary in source {
        let matchup = &salary.game.matchup;
        let game_id = game_of
            .get(&(
                matchup.away.as_str().to_owned(),
                matchup.home.as_str().to_owned(),
            ))
            .ok_or(SalaryImportError::Schedule)?
            .clone();
        let gsis_player_id = if salary.position == DfsPosition::Dst {
            None
        } else {
            let player = salary.player.as_ref().ok_or(SalaryImportError::Schedule)?;
            let fd_id = player.id;
            if let Some((resolved_fd_id, resolved_gsis_id)) = resolved_player
                && resolved_fd_id == fd_id
            {
                Some(resolved_gsis_id.to_owned())
            } else if let Some(crosswalk) = player_store::by_fd_id(db.reader(), player.id).await? {
                Some(crosswalk.gsis_player_id)
            } else {
                let name = format!("{} {}", player.first_name, player.last_name);
                player_index
                    .match_one(&name, salary.team.as_str())
                    .or_else(|| {
                        roster_index.match_one(&name, salary.team.as_str(), salary.position.into())
                    })
            }
        };
        let Some(gsis_player_id) =
            gsis_player_id.or_else(|| (salary.position == DfsPosition::Dst).then(String::new))
        else {
            continue;
        };
        mapped.push((salary, game_id, gsis_player_id));
    }
    db.write_tx::<_, (), sqlx::Error>(async |conn| {
        if let Some((fd_player_id, gsis_player_id)) = resolved_player {
            player_store::upsert(&mut *conn, gsis_player_id, fd_player_id).await?;
        }
        for (salary, game_id, gsis) in &mapped {
            let player_id = salary.player.as_ref().map(|player| player.id);
            let gsis_player_id = if salary.position == DfsPosition::Dst {
                None
            } else {
                player_store::upsert(&mut *conn, gsis, player_id.unwrap()).await?;
                Some(gsis.clone())
            };
            salary_store::upsert(
                &mut *conn,
                &NflPlayerSalary {
                    id: 0,
                    gsis_game_id: game_id.clone(),
                    gsis_player_id,
                    team_abbr: salary.team.as_str().to_owned(),
                    dfs_position: salary.position,
                    salary: i64::from(salary.salary),
                },
            )
            .await?;
        }
        Ok(())
    })
    .await?;
    Ok(())
}

/// Select the next contest week, treating Monday night as belonging to the week just played.
pub fn default_season_week(
    games: &[Game],
    configured_season: Season,
    now: time::OffsetDateTime,
) -> SeasonWeek {
    let now = nfl_data::to_eastern(now);
    let week_start =
        now.date() - time::Duration::days(i64::from(now.weekday().number_days_from_monday()));
    let mut weeks = games
        .iter()
        .filter(|g| {
            g.season == configured_season
                && g.kickoff.is_some_and(|kickoff| {
                    kickoff > now
                        && crate::contests::service::nfl_week_monday(
                            nfl_data::to_eastern(kickoff).date(),
                        ) >= week_start
                })
        })
        .collect::<Vec<_>>();
    weeks.sort_by_key(|game| game.kickoff);
    weeks
        .first()
        .map(|g| SeasonWeek {
            season: g.season,
            season_type: g.season_type,
            week: g.week,
        })
        .unwrap_or(SeasonWeek {
            season: configured_season,
            season_type: SeasonType::Reg,
            week: Week(1),
        })
}

#[cfg(test)]
mod default_week_tests {
    use super::*;
    use time::macros::datetime;

    fn game(id: &str, week: u8, kickoff: time::OffsetDateTime) -> Game {
        Game {
            gsis_game_id: id.into(),
            season: Season(2025),
            week: Week(week),
            season_type: SeasonType::Reg,
            kickoff: Some(kickoff),
            home_team: "KC".parse().unwrap(),
            away_team: "BUF".parse().unwrap(),
            home_score: None,
            away_score: None,
        }
    }

    #[test]
    fn monday_boundary_chooses_the_next_contest_week() {
        let games = vec![
            game("played-mnf", 5, datetime!(2025-10-06 23:15 UTC)),
            game("next", 6, datetime!(2025-10-12 17:00 UTC)),
        ];
        assert_eq!(
            default_season_week(&games, Season(2025), datetime!(2025-10-06 18:00 UTC)).week,
            Week(6)
        );
    }

    #[test]
    fn completed_games_in_the_current_week_do_not_prevent_fallback() {
        let games = vec![game("past", 5, datetime!(2025-10-05 17:00 UTC))];
        assert_eq!(
            default_season_week(&games, Season(2025), datetime!(2025-10-05 23:00 UTC)),
            SeasonWeek {
                season: Season(2025),
                season_type: SeasonType::Reg,
                week: Week(1)
            }
        );
    }

    #[test]
    fn no_future_schedule_falls_back_to_configured_regular_week_one() {
        let games = vec![game("past", 5, datetime!(2025-10-05 17:00 UTC))];
        assert_eq!(
            default_season_week(&games, Season(2026), datetime!(2026-12-01 12:00 UTC)),
            SeasonWeek {
                season: Season(2026),
                season_type: SeasonType::Reg,
                week: Week(1)
            }
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nfl_data::{Game, Player, Season, SeasonType, Week};

    fn game(id: &str, week: u8, away: &str, home: &str) -> Game {
        Game {
            gsis_game_id: id.into(),
            season: Season(2025),
            week: Week(week),
            season_type: SeasonType::Reg,
            kickoff: None,
            home_team: home.parse().unwrap(),
            away_team: away.parse().unwrap(),
            home_score: None,
            away_score: None,
        }
    }

    fn player(gsis: &str, name: &str, team: &str) -> Player {
        Player {
            gsis_id: gsis.into(),
            espn_id: None,
            full_name: name.into(),
            first_name: None,
            last_name: None,
            position: None,
            latest_team: Some(team.parse().unwrap()),
            headshot_url: None,
        }
    }

    const REPRESENTATIVE_CSV: &str =
        include_str!("../../../../fanduel-data/src/csv/fixtures/fanduel-export.csv");

    fn regular_week_2025(week: u8) -> SeasonWeek {
        SeasonWeek {
            season: Season(2025),
            season_type: SeasonType::Reg,
            week: Week(week),
        }
    }

    fn representative_games() -> Vec<Game> {
        vec![
            game("2025_01_BUF_NYJ", 1, "BUF", "NYJ"),
            game("2025_01_NYG_WAS", 1, "NYG", "WAS"),
            game("2025_01_JAX_HOU", 1, "JAX", "HOU"),
            game("2025_01_LA_SEA", 1, "LA", "SEA"),
        ]
    }

    #[tokio::test]
    async fn import_salary_csv_projects_valid_rows_while_returning_unmatched_ids() {
        let db = crate::Db::test(crate::tests::utils::in_memory_pool().await);
        let nfl = nfl_data::NflData::in_memory().await.unwrap();
        nfl.seed_for_test(
            &[
                player("00-1", "Josh Allen", "BUF"),
                player("00-2", "Travis Etienne Jr.", "JAX"),
            ],
            &representative_games(),
        )
        .await
        .unwrap();

        let week = regular_week_2025(1);
        let game_ids = [
            "2025_01_BUF_NYJ",
            "2025_01_NYG_WAS",
            "2025_01_JAX_HOU",
            "2025_01_LA_SEA",
        ];
        let first = import_salary_csv(&db, &nfl, week, REPRESENTATIVE_CSV.as_bytes())
            .await
            .expect("import and project");
        assert_eq!(first.salary_count, 4);
        assert_eq!(first.unmatched.len(), 1);
        assert_eq!(first.unmatched[0].player.as_ref().unwrap().id, 99999);

        let salaries = salary_store::for_games(db.reader(), &game_ids)
            .await
            .unwrap();
        assert_eq!(salaries.len(), 3, "two matched players and DST projected");
        assert!(
            salaries
                .iter()
                .any(|salary| salary.gsis_player_id.as_deref() == Some("00-1"))
        );
        assert!(
            salaries
                .iter()
                .any(|salary| salary.gsis_player_id.as_deref() == Some("00-2"))
        );
        assert!(
            salaries
                .iter()
                .any(|salary| salary.dfs_position == DfsPosition::Dst)
        );
        assert!(!salaries.iter().any(
            |salary| salary.gsis_player_id.is_none() && salary.dfs_position != DfsPosition::Dst
        ));

        let second = import_salary_csv(&db, &nfl, week, REPRESENTATIVE_CSV.as_bytes())
            .await
            .expect("re-import");
        assert_eq!(second.unmatched.len(), 1);
        assert_eq!(
            salary_store::for_games(db.reader(), &game_ids)
                .await
                .unwrap()
                .len(),
            3
        );
    }

    #[tokio::test]
    async fn import_salary_csv_rejects_unscheduled_matchup_without_poisoning_corrected_upload() {
        let db = crate::Db::test(crate::tests::utils::in_memory_pool().await);
        let nfl = nfl_data::NflData::in_memory().await.unwrap();
        // The matchup exists, but only in another week of the same season.
        let mut games = representative_games();
        games.push(game("2025_02_BUF_MIA", 2, "BUF", "MIA"));
        nfl.seed_for_test(
            &[
                player("00-1", "Josh Allen", "BUF"),
                player("00-2", "Travis Etienne Jr.", "JAX"),
            ],
            &games,
        )
        .await
        .unwrap();
        let week = regular_week_2025(1);
        let incorrect_csv = REPRESENTATIVE_CSV.replacen("BUF@NYJ,BUF,NYJ", "BUF@MIA,BUF,MIA", 1);

        assert!(matches!(
            import_salary_csv(&db, &nfl, week, incorrect_csv.as_bytes()).await,
            Err(SalaryImportError::Schedule)
        ));
        assert!(nfl.salaried_players_for(week).await.unwrap().is_empty());
        let game_ids = [
            "2025_01_BUF_NYJ",
            "2025_01_NYG_WAS",
            "2025_01_JAX_HOU",
            "2025_01_LA_SEA",
            "2025_02_BUF_MIA",
        ];
        assert!(
            salary_store::for_games(db.reader(), &game_ids)
                .await
                .unwrap()
                .is_empty()
        );

        let corrected = import_salary_csv(&db, &nfl, week, REPRESENTATIVE_CSV.as_bytes())
            .await
            .expect("corrected upload for the rejected week");
        assert_eq!(corrected.salary_count, 4);
        assert_eq!(corrected.unmatched.len(), 1);
        assert_eq!(nfl.salaried_players_for(week).await.unwrap().len(), 4);
        assert_eq!(
            salary_store::for_games(db.reader(), &game_ids)
                .await
                .unwrap()
                .len(),
            3
        );
        assert!(
            salary_store::for_games(db.reader(), &["2025_02_BUF_MIA"])
                .await
                .unwrap()
                .is_empty()
        );
        let repeated = import_salary_csv(&db, &nfl, week, REPRESENTATIVE_CSV.as_bytes())
            .await
            .expect("corrected upload remains idempotent");
        assert_eq!(repeated, corrected);
        assert_eq!(
            salary_store::for_games(db.reader(), &game_ids)
                .await
                .unwrap()
                .len(),
            3
        );
    }

    #[tokio::test]
    async fn resolve_salary_player_writes_crosswalk_and_projects_salary() {
        let db = crate::Db::test(crate::tests::utils::in_memory_pool().await);
        let nfl = nfl_data::NflData::in_memory().await.unwrap();
        nfl.seed_for_test(&[], &representative_games())
            .await
            .unwrap();
        let week = regular_week_2025(1);
        import_salary_csv(&db, &nfl, week, REPRESENTATIVE_CSV.as_bytes())
            .await
            .expect("initial import");

        let overview = resolve_salary_player(&db, &nfl, week, 99999, "00-3")
            .await
            .expect("resolve and project");
        assert_eq!(overview.unmatched.len(), 2);
        assert!(
            !overview
                .unmatched
                .iter()
                .any(|row| { row.player.as_ref().is_some_and(|player| player.id == 99999) })
        );
        let crosswalk = player_store::by_fd_id(db.reader(), 99999)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(crosswalk.gsis_player_id, "00-3");
        let salaries = salary_store::for_games(db.reader(), &["2025_01_LA_SEA"])
            .await
            .unwrap();
        assert!(
            salaries
                .iter()
                .any(|salary| salary.gsis_player_id.as_deref() == Some("00-3"))
        );
    }

    #[tokio::test]
    async fn resolve_salary_player_rolls_back_crosswalk_when_salary_write_fails() {
        let db = crate::Db::test(crate::tests::utils::in_memory_pool().await);
        let nfl = nfl_data::NflData::in_memory().await.unwrap();
        let week = regular_week_2025(1);
        nfl.seed_for_test(&[], &representative_games())
            .await
            .unwrap();
        import_salary_csv(&db, &nfl, week, REPRESENTATIVE_CSV.as_bytes())
            .await
            .expect("initial import");
        sqlx::query("CREATE TRIGGER reject_salary BEFORE INSERT ON nfl_player_salaries BEGIN SELECT RAISE(FAIL, 'projection rejected'); END")
            .execute(db.reader())
            .await
            .unwrap();

        assert!(
            resolve_salary_player(&db, &nfl, week, 99999, "00-3")
                .await
                .is_err()
        );
        assert!(
            player_store::by_fd_id(db.reader(), 99999)
                .await
                .unwrap()
                .is_none()
        );
    }
}
