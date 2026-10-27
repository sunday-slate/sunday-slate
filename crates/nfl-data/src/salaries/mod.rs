pub(crate) mod candidates;
pub(crate) mod error;
mod matcher;
mod schedule;

pub use error::SalaryError;
pub use schedule::resolve_slate_games;

use std::collections::{BTreeSet, HashMap};

use crate::salaries::matcher::{PlayerIndex, RosterIndex};
use crate::store::PlayerLink;
use crate::{DfsPosition, NflData, SalariedPlayer, SeasonWeek};

/// One stored contest week and the source player IDs still needing a crosswalk.
pub struct SalaryWeekOverview {
    pub season_week: SeasonWeek,
    pub salary_count: usize,
    pub unmatched: Vec<SalariedPlayer>,
}

/// Salary row with its resolved GSIS game and player. Test helper only.
#[derive(Debug, Clone, PartialEq)]
#[allow(dead_code)]
pub struct ReconciledSalary {
    pub gsis_game_id: String,
    pub gsis_player_id: Option<String>,
    pub team: crate::TeamAbbr,
    pub position: DfsPosition,
    pub salary: u32,
}

pub(crate) fn salary_matchups(source: &[SalariedPlayer]) -> BTreeSet<(String, String)> {
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

impl NflData {
    /// Match the week's stored source rows and persist successful links in
    /// one transaction. Precedence: persisted link > `PlayerIndex` >
    /// `RosterIndex`; no match leaves the record unresolved.
    pub(crate) async fn reconcile_salary_week(
        &self,
        season_week: SeasonWeek,
    ) -> Result<(), SalaryError> {
        let source = self.fanduel.salaried_players_for(season_week).await?;
        let matchups = salary_matchups(&source);
        let games = self.games(season_week.season).await?;
        let slate_games = resolve_slate_games(&games, &season_week, &matchups)
            .map_err(|_| SalaryError::Schedule)?;
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
        let players = self.players().await?;
        let player_index = PlayerIndex::build(&players);
        let mut roster_entries = Vec::new();
        let mut teams = BTreeSet::new();
        for game in &slate_games {
            teams.insert(game.home_team);
            teams.insert(game.away_team);
        }
        for team in teams {
            roster_entries.extend(
                self.weekly_roster(season_week.season, season_week.week, &team)
                    .await?,
            );
        }
        let roster_index = RosterIndex::build(&roster_entries);
        let links = self.store.links().await?;

        let mut mapped = Vec::new();
        for salary in source {
            let matchup = &salary.game.matchup;
            let game_id = game_of
                .get(&(
                    matchup.away.as_str().to_owned(),
                    matchup.home.as_str().to_owned(),
                ))
                .ok_or(SalaryError::Schedule)?
                .clone();
            let gsis_player_id: Option<String> = if salary.position == DfsPosition::Dst {
                None
            } else {
                let player = salary.player.as_ref().ok_or(SalaryError::Schedule)?;
                let fd_id = player.id;
                if let Some(gsis) = links.get(&fd_id) {
                    Some(gsis.clone())
                } else {
                    let name = format!("{} {}", player.first_name, player.last_name);
                    player_index
                        .match_one(&name, salary.team.as_str())
                        .or_else(|| {
                            roster_index.match_one(
                                &name,
                                salary.team.as_str(),
                                salary.position.into(),
                            )
                        })
                }
            };
            if let Some(gsis_player_id) = gsis_player_id {
                let fd_id = salary.player.as_ref().expect("player row").id;
                mapped.push(PlayerLink {
                    fd_player_id: fd_id,
                    gsis_player_id,
                });
            }
            let _ = game_id;
        }
        self.store.save_links(&mapped).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Game, Player, Season, SeasonType, Week, WeeklyRosterEntry};

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
        include_str!("../../../fanduel-data/src/csv/fixtures/fanduel-export.csv");

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
    async fn import_salary_csv_stores_source_rows_and_links_matches() {
        let nfl = NflData::in_memory().await.unwrap();
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
        let first = nfl
            .import_salary_csv(week, REPRESENTATIVE_CSV.as_bytes())
            .await
            .expect("import");
        assert_eq!(first.salary_count, 4);
        assert_eq!(first.unmatched.len(), 1);
        assert_eq!(first.unmatched[0].player.as_ref().unwrap().id, 99999);

        let salaries = nfl
            .salaries_for_games(&representative_games())
            .await
            .unwrap();
        assert_eq!(salaries.len(), 3, "two matched players and DST");
        assert!(
            salaries
                .iter()
                .any(|s| s.gsis_player_id.as_deref() == Some("00-1"))
        );
        assert!(
            salaries
                .iter()
                .any(|s| s.gsis_player_id.as_deref() == Some("00-2"))
        );
        assert!(salaries.iter().any(|s| s.position == DfsPosition::Dst));

        let second = nfl
            .import_salary_csv(week, REPRESENTATIVE_CSV.as_bytes())
            .await
            .expect("re-import");
        assert_eq!(second.unmatched.len(), 1);
        assert_eq!(
            nfl.salaries_for_games(&representative_games())
                .await
                .unwrap()
                .len(),
            3
        );
    }

    #[tokio::test]
    async fn import_salary_csv_rejects_unscheduled_matchup_without_poisoning_corrected_upload() {
        let nfl = NflData::in_memory().await.unwrap();
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
            nfl.import_salary_csv(week, incorrect_csv.as_bytes()).await,
            Err(SalaryError::Schedule)
        ));
        assert!(
            nfl.fanduel
                .salaried_players_for(week)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(nfl.salaries_for_games(&games).await.unwrap().is_empty());

        let corrected = nfl
            .import_salary_csv(week, REPRESENTATIVE_CSV.as_bytes())
            .await
            .expect("corrected upload for the rejected week");
        assert_eq!(corrected.salary_count, 4);
        assert_eq!(corrected.unmatched.len(), 1);
        assert_eq!(
            nfl.fanduel.salaried_players_for(week).await.unwrap().len(),
            4
        );
        assert_eq!(nfl.salaries_for_games(&games).await.unwrap().len(), 3);
        assert!(
            nfl.salaries_for_games(&[game("2025_02_BUF_MIA", 2, "BUF", "MIA")])
                .await
                .unwrap()
                .is_empty()
        );
        let repeated = nfl
            .import_salary_csv(week, REPRESENTATIVE_CSV.as_bytes())
            .await
            .expect("corrected upload remains idempotent");
        assert_eq!(repeated.salary_count, corrected.salary_count);
        assert_eq!(repeated.unmatched, corrected.unmatched);
        assert_eq!(nfl.salaries_for_games(&games).await.unwrap().len(), 3);
    }

    #[tokio::test]
    async fn resolve_salary_player_writes_crosswalk_and_reconciles() {
        let nfl = NflData::in_memory().await.unwrap();
        nfl.seed_for_test(&[], &representative_games())
            .await
            .unwrap();
        let week = regular_week_2025(1);
        nfl.import_salary_csv(week, REPRESENTATIVE_CSV.as_bytes())
            .await
            .expect("initial import");

        let overview = nfl
            .resolve_salary_player(week, 99999, "00-3")
            .await
            .expect("resolve");
        assert_eq!(overview.unmatched.len(), 2);
        assert!(
            !overview
                .unmatched
                .iter()
                .any(|row| row.player.as_ref().is_some_and(|player| player.id == 99999))
        );
        let link = nfl.player_link_by_fd_id(99999).await.unwrap().unwrap();
        assert_eq!(link.gsis_player_id, "00-3");
        let salaries = nfl
            .salaries_for_games(&[game("2025_01_LA_SEA", 1, "LA", "SEA")])
            .await
            .unwrap();
        assert!(
            salaries
                .iter()
                .any(|s| s.gsis_player_id.as_deref() == Some("00-3"))
        );
    }

    #[tokio::test]
    async fn link_applies_globally_without_reimport_and_price_updates_flow_through() {
        let nfl = NflData::in_memory().await.unwrap();
        let roster_only = WeeklyRosterEntry {
            season: Season(2025),
            week: Week(1),
            team: "BUF".parse().unwrap(),
            gsis_id: Some("00-1".into()),
            espn_id: None,
            full_name: "Josh Allen".into(),
            last_name: None,
            position: Some("QB".into()),
        };
        nfl.seed_for_test(
            &[],
            &[
                game("2025_01_BUF_NYJ", 1, "BUF", "NYJ"),
                game("2025_01_NYG_WAS", 1, "NYG", "WAS"),
                game("2025_01_JAX_HOU", 1, "JAX", "HOU"),
                game("2025_01_LA_SEA", 1, "LA", "SEA"),
                game("2025_02_BUF_MIA", 2, "BUF", "MIA"),
                game("2025_02_NYG_WAS", 2, "NYG", "WAS"),
                game("2025_02_JAX_HOU", 2, "JAX", "HOU"),
                game("2025_02_LA_SEA", 2, "LA", "SEA"),
            ],
        )
        .await
        .unwrap();
        nfl.seed_weekly_roster_for_test(&[roster_only])
            .await
            .unwrap();

        let week1 = regular_week_2025(1);
        let week2 = regular_week_2025(2);
        let week2_game = game("2025_02_BUF_MIA", 2, "BUF", "MIA");

        let week2_csv = REPRESENTATIVE_CSV.replacen("BUF@NYJ,BUF,NYJ", "BUF@MIA,BUF,MIA", 1);
        nfl.import_salary_csv(week2, week2_csv.as_bytes())
            .await
            .unwrap();
        let before = nfl
            .salaries_for_games(std::slice::from_ref(&week2_game))
            .await
            .unwrap();
        assert!(
            !before
                .iter()
                .any(|s| s.gsis_player_id.as_deref() == Some("00-1"))
        );
        assert!(
            !nfl.has_salaries_for_games(std::slice::from_ref(&week2_game))
                .await
                .unwrap()
        );
        assert!(
            nfl.salary_week_overview(week2)
                .await
                .unwrap()
                .unmatched
                .iter()
                .any(|row| row.player.as_ref().is_some_and(|p| p.id == 62239))
        );

        let overview = nfl
            .resolve_salary_player(week2, 62239, "00-1")
            .await
            .expect("resolve week two");
        assert_eq!(overview.unmatched.len(), 2);
        assert!(
            !overview
                .unmatched
                .iter()
                .any(|row| row.player.as_ref().is_some_and(|p| p.id == 62239))
        );

        assert_eq!(
            nfl.player_link_by_fd_id(62239)
                .await
                .unwrap()
                .unwrap()
                .gsis_player_id,
            "00-1"
        );

        nfl.import_salary_csv(week1, REPRESENTATIVE_CSV.as_bytes())
            .await
            .unwrap();
        let week1_rows = nfl
            .salaries_for_games(&[game("2025_01_BUF_NYJ", 1, "BUF", "NYJ")])
            .await
            .unwrap();
        assert!(
            week1_rows
                .iter()
                .any(|s| s.gsis_player_id.as_deref() == Some("00-1"))
        );

        let mut updated = nfl.fanduel.salaried_players_for(week2).await.unwrap();
        for row in &mut updated {
            if row.player.as_ref().is_some_and(|p| p.id == 62239) {
                row.salary = 9000;
            }
        }
        nfl.save_fanduel_salaries(&updated).await.unwrap();
        let priced = nfl
            .salaries_for_games(std::slice::from_ref(&week2_game))
            .await
            .unwrap();
        assert_eq!(priced.len(), 1);
        assert_eq!(priced[0].salary, 9000);
        assert_eq!(priced[0].gsis_player_id.as_deref(), Some("00-1"));

        assert_eq!(
            nfl.fanduel.salaried_players_for(week2).await.unwrap().len(),
            4
        );
        assert_eq!(
            nfl.salary_week_overview(week2)
                .await
                .unwrap()
                .unmatched
                .len(),
            2
        );
    }
}
