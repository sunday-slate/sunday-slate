pub(crate) mod candidates;
pub(crate) mod error;
mod matcher;
mod schedule;

pub use error::SalaryError;
pub use schedule::resolve_slate_games;

use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::WeeklySummary;
use crate::salaries::matcher::{PlayerIndex, RosterIndex};
use crate::store::PlayerLink;
use crate::{DfsPosition, Game, NflData, SalariedPlayer, SeasonWeek};

/// One stored contest week and the source player IDs still needing a crosswalk.
pub struct SalaryWeekOverview {
    pub season_week: SeasonWeek,
    pub salary_count: usize,
    pub unmatched: Vec<SalariedPlayer>,
}

/// A salary row attached to a selected GSIS game, read from the authoritative
/// source with persisted player links applied. DST rows carry no player id.
#[derive(Debug, Clone, PartialEq)]
pub struct ReconciledSalary {
    pub gsis_game_id: String,
    pub gsis_player_id: Option<String>,
    pub team: crate::TeamAbbr,
    pub position: DfsPosition,
    pub salary: u32,
}

impl NflData {
    /// Validate the typed CSV's schedule, persist it, and (re)apply persisted
    /// player links. CSV parsing, source save, and omitted-row upsert behavior
    /// are the source store's; link persistence only writes successful links.
    pub async fn import_salary_csv(
        &self,
        season_week: SeasonWeek,
        csv_data: &[u8],
    ) -> Result<SalaryWeekOverview, SalaryError> {
        let source = self.parse_fanduel_csv(season_week, csv_data)?;
        let games = self.games(season_week.season).await?;
        resolve_slate_games(&games, &season_week, &salary_matchups(&source))
            .map_err(|_| SalaryError::Schedule)?;
        self.save_fanduel_salaries(&source).await?;
        self.reconcile_salary_week(season_week, None).await?;
        self.salary_week_overview(season_week).await
    }

    /// Read the stored week's overview: every source row plus the player IDs
    /// still lacking a persisted link. DST rows are never unmatched.
    pub async fn salary_week_overview(
        &self,
        season_week: SeasonWeek,
    ) -> Result<SalaryWeekOverview, SalaryError> {
        let salaries = self.fanduel.salaried_players_for(season_week).await?;
        let salary_count = salaries.len();

        let links = self.store.links().await?;
        let unmatched = salaries
            .into_iter()
            .filter(|salaried_player| match &salaried_player.player {
                Some(player) => !links.contains_key(&player.id),
                None => false,
            })
            .collect();

        Ok(SalaryWeekOverview {
            season_week,
            salary_count,
            unmatched,
        })
    }

    /// Confirm an explicit player link for one source row and re-reconcile the
    /// whole stored week. The source row must exist in the requested week.
    pub async fn resolve_salary_player(
        &self,
        season_week: SeasonWeek,
        fd_player_id: u32,
        gsis_player_id: &str,
    ) -> Result<SalaryWeekOverview, SalaryError> {
        let source = self.fanduel.salaried_players_for(season_week).await?;
        if !source.iter().any(|salary| {
            salary
                .player
                .as_ref()
                .is_some_and(|player| player.id == fd_player_id)
        }) {
            return Err(SalaryError::Schedule);
        }
        self.reconcile_salary_week(season_week, Some((fd_player_id, gsis_player_id)))
            .await?;
        self.salary_week_overview(season_week).await
    }

    /// The persisted link for one FanDuel player id, when linked.
    pub async fn player_link_by_fd_id(
        &self,
        fd_player_id: u32,
    ) -> Result<Option<PlayerLink>, SalaryError> {
        self.store.by_fd_id(fd_player_id).await
    }

    /// Persisted GSIS ids, deduplicated and sorted. Sourced from links only.
    pub async fn reconciled_gsis_ids(&self) -> Result<Vec<String>, SalaryError> {
        self.store.gsis_ids().await
    }

    /// Per-week source counts with per-week unresolved counts, across every
    /// stored week. DST entries are never unresolved.
    pub async fn salary_week_summaries(&self) -> Result<Vec<WeeklySummary>, SalaryError> {
        let link_ids = self.store.links().await?;
        let matched_ids: Vec<u32> = link_ids.keys().copied().collect();
        let salary_counts = self.fanduel.weekly_salary_counts().await?;
        let unmatched_counts = self.fanduel.weekly_unmatched_counts(&matched_ids).await?;

        let summaries = salary_counts
            .into_iter()
            .map(|(season_week, salary_count)| WeeklySummary {
                season_week,
                salary_count,
                unmatched_count: unmatched_counts.get(&season_week).copied().unwrap_or(0),
            })
            .collect();

        Ok(summaries)
    }

    /// Reconciled salaries for the given games, ordered by game id then
    /// descending salary. Reads each selected week once and loads the link map
    /// once; never matches or writes, so a fresh link immediately affects all
    /// stored weeks and updated source prices need no projection step.
    pub async fn salaries_for_games(
        &self,
        games: &[Game],
    ) -> Result<Vec<ReconciledSalary>, SalaryError> {
        if games.is_empty() {
            return Ok(Vec::new());
        }

        // Deduplicate selected games by id, grouping their matchups per week.
        let mut by_week: BTreeMap<SeasonWeek, BTreeSet<(String, String)>> = BTreeMap::new();
        let mut selected: HashMap<&str, ()> = HashMap::new();
        for game in games {
            selected.insert(game.gsis_game_id.as_str(), ());
            by_week
                .entry(SeasonWeek {
                    season: game.season,
                    season_type: game.season_type,
                    week: game.week,
                })
                .or_default()
                .insert((
                    game.away_team.as_str().to_owned(),
                    game.home_team.as_str().to_owned(),
                ));
        }

        let links = self.store.links().await?;
        let mut out: Vec<ReconciledSalary> = Vec::new();
        for (season_week, _matchups) in by_week {
            let rows = self.fanduel.salaried_players_for(season_week).await?;
            for game in games.iter().filter(|g| {
                g.season == season_week.season
                    && g.season_type == season_week.season_type
                    && g.week == season_week.week
            }) {
                let id = game.gsis_game_id.as_str();
                for salary in &rows {
                    if salary.game.matchup.away.as_str() != game.away_team.as_str()
                        || salary.game.matchup.home.as_str() != game.home_team.as_str()
                    {
                        continue;
                    }
                    let gsis_player_id = match &salary.player {
                        Some(player) => match links.get(&player.id) {
                            Some(gsis) => Some(gsis.clone()),
                            None => continue,
                        },
                        None if salary.position == DfsPosition::Dst => None,
                        None => continue,
                    };
                    out.push(ReconciledSalary {
                        gsis_game_id: id.to_owned(),
                        gsis_player_id,
                        team: salary.team,
                        position: salary.position,
                        salary: salary.salary,
                    });
                }
            }
        }
        out.sort_by(|a, b| {
            a.gsis_game_id
                .cmp(&b.gsis_game_id)
                .then_with(|| b.salary.cmp(&a.salary))
        });
        Ok(out)
    }

    /// Whether any reconciled salary exists for the given games. Exits upon
    /// the first usable record without building a full result vector.
    pub async fn has_salaries_for_games(&self, games: &[Game]) -> Result<bool, SalaryError> {
        if games.is_empty() {
            return Ok(false);
        }

        let mut by_week: BTreeMap<SeasonWeek, BTreeSet<(String, String)>> = BTreeMap::new();
        for game in games {
            by_week
                .entry(SeasonWeek {
                    season: game.season,
                    season_type: game.season_type,
                    week: game.week,
                })
                .or_default()
                .insert((
                    game.away_team.as_str().to_owned(),
                    game.home_team.as_str().to_owned(),
                ));
        }

        let links = self.store.links().await?;
        for (season_week, _matchups) in by_week {
            let rows = self.fanduel.salaried_players_for(season_week).await?;
            for game in games.iter().filter(|g| {
                g.season == season_week.season
                    && g.season_type == season_week.season_type
                    && g.week == season_week.week
            }) {
                for salary in &rows {
                    if salary.game.matchup.away.as_str() != game.away_team.as_str()
                        || salary.game.matchup.home.as_str() != game.home_team.as_str()
                    {
                        continue;
                    }
                    let usable = match &salary.player {
                        Some(player) => links.contains_key(&player.id),
                        None => salary.position == DfsPosition::Dst,
                    };
                    if usable {
                        return Ok(true);
                    }
                }
            }
        }
        Ok(false)
    }
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

impl NflData {
    /// Match every stored source row for the week and apply successful links
    /// in one store transaction. Never writes salaries. Precedence: the
    /// explicit selection > a persisted link > `PlayerIndex` > `RosterIndex`.
    /// A `None` match simply contributes no link; its FanDuel record remains.
    async fn reconcile_salary_week(
        &self,
        season_week: SeasonWeek,
        resolved_player: Option<(u32, &str)>,
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
                if let Some((resolved_fd_id, resolved_gsis_id)) = resolved_player
                    && resolved_fd_id == fd_id
                {
                    Some(resolved_gsis_id.to_owned())
                } else if let Some(gsis) = links.get(&fd_id) {
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
            // DST rows carry no player link; a player row with no match simply
            // contributes no link and keeps its FanDuel record.
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
        // The roster-only identity 00-1 cannot be named from the players table,
        // so it never auto-matches: the import leaves it unresolved in both weeks.
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

        // Store source records before a link exists: the application pool
        // omits the player while the week overview lists it.
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

        // The other source rows (Etienne, the Wideout, the DST) were never
        // seeded as identities, so two player rows remain unresolved and the
        // resolved row leaves the overview.
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

        // Read calls leave link contents unchanged.
        assert_eq!(
            nfl.player_link_by_fd_id(62239)
                .await
                .unwrap()
                .unwrap()
                .gsis_player_id,
            "00-1"
        );

        // Re-importing another week does not erase the link, and the persisted
        // link immediately affects every stored week without matching again.
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

        // Update a source amount through the existing source API: the next
        // reconciled read returns the new exact amount without invoking matching.
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

        // Source rows and unmatched display persist throughout.
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

    #[tokio::test]
    async fn identical_reg_and_post_matchups_return_only_the_selected_week() {
        let nfl = NflData::in_memory().await.unwrap();
        let set_type = |week: u8, season_type: SeasonType, id: &str| Game {
            season_type,
            gsis_game_id: id.into(),
            ..game("x", week, "BUF", "KC")
        };
        let reg = set_type(1, SeasonType::Reg, "2025_01_BUF_KC");
        let post = set_type(21, SeasonType::Post, "2025_21_BUF_KC");
        let reg_week = SeasonWeek {
            season: Season(2025),
            season_type: SeasonType::Reg,
            week: Week(1),
        };
        let post_week = SeasonWeek {
            season: Season(2025),
            season_type: SeasonType::Post,
            week: Week(21),
        };
        nfl.seed_for_test(
            &[player("00-1", "Josh Allen", "BUF")],
            &[
                reg.clone(),
                post.clone(),
                game("2025_01_NYG_WAS", 1, "NYG", "WAS"),
                game("2025_01_JAX_HOU", 1, "JAX", "HOU"),
                game("2025_01_LA_SEA", 1, "LA", "SEA"),
                Game {
                    season_type: SeasonType::Post,
                    gsis_game_id: "2025_21_NYG_WAS".into(),
                    ..game("x", 21, "NYG", "WAS")
                },
                Game {
                    season_type: SeasonType::Post,
                    gsis_game_id: "2025_21_JAX_HOU".into(),
                    ..game("x", 21, "JAX", "HOU")
                },
                Game {
                    season_type: SeasonType::Post,
                    gsis_game_id: "2025_21_LA_SEA".into(),
                    ..game("x", 21, "LA", "SEA")
                },
            ],
        )
        .await
        .unwrap();

        let csv = REPRESENTATIVE_CSV.replacen("BUF@NYJ,BUF,NYJ", "BUF@KC,BUF,KC", 1);
        nfl.import_salary_csv(reg_week, csv.as_bytes())
            .await
            .unwrap();
        nfl.import_salary_csv(post_week, csv.as_bytes())
            .await
            .unwrap();

        // Each read returns only the rows for its own week's game. The fixture's
        // only BUF-row is the QB (its DST row is New York's), so each week's
        // BUF@KC game yields exactly the linked player.
        let reg_rows = nfl
            .salaries_for_games(std::slice::from_ref(&reg))
            .await
            .unwrap();
        let post_rows = nfl
            .salaries_for_games(std::slice::from_ref(&post))
            .await
            .unwrap();
        assert_eq!(reg_rows.len(), 1, "linked player only");
        assert!(
            reg_rows
                .iter()
                .all(|row| row.gsis_game_id == "2025_01_BUF_KC")
        );
        assert_eq!(post_rows.len(), 1, "linked player only");
        assert!(
            post_rows
                .iter()
                .all(|row| row.gsis_game_id == "2025_21_BUF_KC")
        );
    }

    #[tokio::test]
    async fn empty_games_and_unlinked_players_yield_empty_and_false() {
        let nfl = NflData::in_memory().await.unwrap();
        nfl.seed_for_test(&[], &representative_games())
            .await
            .unwrap();
        nfl.import_salary_csv(regular_week_2025(1), REPRESENTATIVE_CSV.as_bytes())
            .await
            .unwrap();

        // Empty games → empty salaries, false.
        assert!(nfl.salaries_for_games(&[]).await.unwrap().is_empty());
        assert!(!nfl.has_salaries_for_games(&[]).await.unwrap());

        // Keep only the fixture's unlinked Rookie row plus its own NYG@WAS DST:
        // the BUF@NYJ game has no usable record, so the read is false.
        let all = nfl
            .parse_fanduel_csv(regular_week_2025(1), REPRESENTATIVE_CSV.as_bytes())
            .unwrap();
        let rookie = all
            .iter()
            .find(|row| row.player.as_ref().is_some_and(|p| p.id == 99999))
            .expect("Rookie row in fixture")
            .clone();
        let dst = all
            .iter()
            .find(|row| row.position == DfsPosition::Dst && row.team.as_str() == "NYG")
            .expect("NYG DST row in fixture")
            .clone();
        nfl.save_fanduel_salaries(&[rookie.clone(), dst])
            .await
            .unwrap();
        assert!(
            !nfl.has_salaries_for_games(&[game("2025_01_BUF_NYJ", 1, "BUF", "NYJ")])
                .await
                .unwrap()
        );

        // Dropping the DST row leaves the game unlinked-only: still false.
        nfl.save_fanduel_salaries(&[rookie]).await.unwrap();
        assert!(
            !nfl.has_salaries_for_games(&[game("2025_01_BUF_NYJ", 1, "BUF", "NYJ")])
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn reconciled_salaries_sort_by_game_then_descending_salary() {
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
        nfl.import_salary_csv(regular_week_2025(1), REPRESENTATIVE_CSV.as_bytes())
            .await
            .unwrap();

        let rows = nfl
            .salaries_for_games(&[
                game("2025_01_JAX_HOU", 1, "JAX", "HOU"),
                game("2025_01_BUF_NYJ", 1, "BUF", "NYJ"),
            ])
            .await
            .unwrap();
        let ids: Vec<&str> = rows.iter().map(|row| row.gsis_game_id.as_str()).collect();
        assert_eq!(ids, ["2025_01_BUF_NYJ", "2025_01_JAX_HOU"]);
        for pair in rows.windows(2) {
            if pair[0].gsis_game_id == pair[1].gsis_game_id {
                assert!(pair[0].salary >= pair[1].salary);
            }
        }
    }
}
