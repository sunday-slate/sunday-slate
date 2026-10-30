pub(crate) mod candidates;
mod matcher;
mod schedule;

pub use schedule::resolve_slate_games;

use std::collections::{BTreeSet, HashMap};

use crate::salaries::matcher::{PlayerIndex, RosterIndex};
use crate::store::{PlayerLink, player_link};
use crate::{DfsPosition, NflData, NflDataError, SalariedPlayer, SeasonWeek};

/// One stored contest week and the source player IDs still needing a crosswalk.
pub struct SalaryWeekOverview {
    pub season_week: SeasonWeek,
    pub salary_count: usize,
    pub unmatched: Vec<SalariedPlayer>,
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
    ) -> Result<(), NflDataError> {
        let source = self.fanduel.salaried_players_for(season_week).await?;
        let matchups = salary_matchups(&source);
        let games = self.games(season_week.season).await?;
        let slate_games = resolve_slate_games(&games, &season_week, &matchups)
            .map_err(|_| NflDataError::Schedule)?;
        let game_of: HashMap<(String, String), String> = slate_games
            .iter()
            .map(|game| {
                (
                    (
                        game.away_team.as_str().to_owned(),
                        game.home_team.as_str().to_owned(),
                    ),
                    game.game_id.clone(),
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
        let links = player_link::links(self.store.reader()).await?;

        let mut mapped = Vec::new();
        for salary in source {
            let matchup = &salary.game.matchup;
            let game_id = game_of
                .get(&(
                    matchup.away.as_str().to_owned(),
                    matchup.home.as_str().to_owned(),
                ))
                .ok_or(NflDataError::Schedule)?
                .clone();
            let gsis_player_id: Option<String> = if salary.position == DfsPosition::Dst {
                None
            } else {
                let player = salary.player.as_ref().ok_or(NflDataError::Schedule)?;
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
        player_link::save_links(&self.store, &mapped).await?;
        Ok(())
    }
}
