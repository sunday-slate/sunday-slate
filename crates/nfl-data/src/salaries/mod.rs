pub(crate) mod candidates;
mod matcher;
mod schedule;

pub use schedule::resolve_slate_games;

use std::collections::{BTreeSet, HashMap};

use crate::salaries::matcher::{PlayerIndex, RosterIndex};
use crate::store::{PlayerLink, Store, player_link};
use crate::{DfsPosition, NflDataError, SeasonWeek};
use fanduel_data::{FanduelData, SalariedPlayer};
use nflverse_data::NflverseData;

/// One stored contest week and the source player IDs still needing a crosswalk.
pub(crate) struct SalaryWeekOverview {
    pub season_week: SeasonWeek,
    pub salary_count: usize,
    pub unmatched: Vec<SalariedPlayer>,
}

/// One stored contest week's source and unmatched-row counts.
pub(crate) struct WeeklySummary {
    pub(crate) season_week: SeasonWeek,
    pub(crate) salary_count: usize,
    pub(crate) unmatched_count: usize,
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

pub(crate) fn parse_fanduel_csv(
    fanduel: &FanduelData,
    season_week: SeasonWeek,
    csv_data: &[u8],
) -> Result<Vec<SalariedPlayer>, NflDataError> {
    fanduel
        .parse_fanduel_csv(season_week, csv_data)
        .map_err(NflDataError::Fanduel)
}

/// Persist typed FanDuel source rows. Callers republish the snapshot at
/// their workflow boundary, once the whole import is persisted.
pub(crate) async fn save_fanduel_salaries(
    fanduel: &FanduelData,
    salaries: &[SalariedPlayer],
) -> Result<(), NflDataError> {
    fanduel
        .save_salaried_players(salaries)
        .await
        .map_err(NflDataError::Fanduel)
}

pub(crate) async fn fanduel_weekly_summaries(
    fanduel: &FanduelData,
    matched_ids: &[u32],
) -> Result<Vec<WeeklySummary>, NflDataError> {
    let salary_counts = fanduel.weekly_salary_counts().await?;
    let unmatched_counts = fanduel.weekly_unmatched_counts(matched_ids).await?;

    let summaries = salary_counts
        .into_iter()
        .map(|(season_week, salary_count)| {
            let unmatched_count = unmatched_counts.get(&season_week).copied().unwrap_or(0);
            WeeklySummary {
                season_week,
                salary_count,
                unmatched_count,
            }
        })
        .collect();

    Ok(summaries)
}

/// Match the week's stored source rows and persist successful links in one
/// transaction. Precedence: persisted link > `PlayerIndex` > `RosterIndex`; no
/// match leaves the record unresolved.
pub(crate) async fn reconcile_salary_week(
    fanduel: &FanduelData,
    nflverse: &NflverseData,
    store: &Store,
    season_week: SeasonWeek,
) -> Result<(), NflDataError> {
    let source = fanduel.salaried_players_for(season_week).await?;
    let matchups = salary_matchups(&source);
    let games = crate::nflverse::games(nflverse, season_week.season).await?;
    let slate_games =
        resolve_slate_games(&games, &season_week, &matchups).map_err(|_| NflDataError::Schedule)?;
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
    let players = nflverse.players().await.map_err(NflDataError::Nflverse)?;
    let espn_by_gsis: HashMap<&str, &str> = players
        .iter()
        .filter_map(|player| {
            player
                .espn_id
                .as_deref()
                .map(str::trim)
                .filter(|espn| !espn.is_empty())
                .map(|espn| (player.gsis_id.as_str(), espn))
        })
        .collect();
    let player_index = PlayerIndex::build(&players);
    let mut roster_entries = Vec::new();
    let mut teams = BTreeSet::new();
    for game in &slate_games {
        teams.insert(game.home_team);
        teams.insert(game.away_team);
    }
    for team in teams {
        roster_entries.extend(
            nflverse
                .weekly_roster(season_week, &team)
                .await
                .map_err(NflDataError::Nflverse)?,
        );
    }
    let roster_index = RosterIndex::build(&roster_entries);
    let links = player_link::links(store.reader()).await?;

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
                        roster_index.match_one(&name, salary.team.as_str(), salary.position.into())
                    })
            }
        };
        if let Some(gsis_player_id) = gsis_player_id {
            let fd_id = salary.player.as_ref().expect("player row").id;
            mapped.push(PlayerLink {
                fd_player_id: fd_id,
                espn_player_id: espn_by_gsis
                    .get(gsis_player_id.as_str())
                    .map(|espn| (*espn).to_owned()),
                gsis_player_id,
            });
        }
        let _ = game_id;
    }
    player_link::save_player_links(store, &mapped).await?;
    Ok(())
}
