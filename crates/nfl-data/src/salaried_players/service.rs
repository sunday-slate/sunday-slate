use crate::salaried_players::SalariedPlayer;
use crate::slates::{self, SlateId};
use crate::store::player_link;
use crate::{NflData, NflDataError, PlayerIdentity, scoring};
use fanduel_data::Matchup;
use nfl_model::{DfsPositionSlotType, Game, NflPlayerId};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

pub async fn for_position(
    nfl: &NflData,
    slate_id: &SlateId,
    pos: DfsPositionSlotType,
) -> Result<Vec<SalariedPlayer>, NflDataError> {
    if let Some(slate) = slates::by_id(nfl, slate_id).await? {
        let fd_salaried_players: Vec<fanduel_data::SalariedPlayer> = nfl
            .fanduel
            .salaried_players_for(slate.season_week)
            .await?
            .into_iter()
            .filter(|fd_player| pos.allows(fd_player.position))
            .collect();

        // map FD games to canonical Games in the Slate
        let fd_games: Vec<fanduel_data::Game> =
            fd_salaried_players.iter().map(|p| p.game.clone()).collect();
        let fd_game_map = fd_game_map(nfl, slate_id, &fd_games).await?;

        // remove any fd players not in the slate
        let fd_salaried_players: Vec<fanduel_data::SalariedPlayer> = fd_salaried_players
            .into_iter()
            .filter(|p| !fd_game_map.contains_key(&p.game.matchup))
            .collect();

        // map FanDuel IDs to Nflverse gsis IDs
        let fd_ids: Vec<u32> = fd_salaried_players
            .iter()
            .filter_map(|sp| sp.player.as_ref().map(|p| p.id))
            .collect();
        let player_id_map: HashMap<u32, NflPlayerId> =
            player_link::links_for_fd_ids(nfl.store.reader(), &fd_ids)
                .await?
                .into_iter()
                .map(|(fd_id, gsis_id)| (fd_id, NflPlayerId(gsis_id)))
                .collect();

        // look up player identities from nflverse
        let id_str: Vec<String> = player_id_map.values().map(|id| id.0.clone()).collect();
        let player_ident: HashMap<NflPlayerId, PlayerIdentity> = nfl
            .identify(&id_str)
            .await?
            .into_iter()
            .map(|(gsis_id, ident)| (NflPlayerId(gsis_id), ident))
            .collect();

        //season ffpg
        let season_averages = scoring::season_averages(
            nfl,
            slate.season_week.season,
            &id_str,
            slate.season_week.week,
        )
        .await?;

        let players: Vec<SalariedPlayer> = fd_salaried_players
            .into_iter()
            .filter_map(|fd_salaried_player| {
                if let Some(game) = fd_game_map.get(&fd_salaried_player.game.matchup)
                    && let Some(fd_player_id) =
                        fd_salaried_player.player.map(|fd_player| fd_player.id)
                        && let Some(nfl_player_id) = player_id_map.get(&fd_player_id)
                            && let Some(player_identity) = player_ident.get(nfl_player_id) {
                                let (games_played, fppg) = season_averages
                                    .players
                                    .get(nfl_player_id)
                                    .map(|ppg| (ppg.games_played, ppg.avg_points))
                                    .unwrap_or_default();

                                return Some(SalariedPlayer {
                                    id: Some(nfl_player_id.clone()),
                                    game: game.clone(),
                                    name: player_identity.name.clone(),
                                    position: fd_salaried_player.position,
                                    team: fd_salaried_player.team,
                                    salary: fd_salaried_player.salary,
                                    games_played,
                                    fppg,
                                    injury: None,
                                });
                            }
                None
            })
            .collect();
        Ok(players)
    } else {
        Ok(vec![])
    }
}

async fn fd_game_map(
    nfl: &NflData,
    slate_id: &SlateId,
    fd_games: &[fanduel_data::Game],
) -> Result<HashMap<Matchup, Arc<Game>>, NflDataError> {
    let games = slates::games(nfl, slate_id).await?;
    let matchups: HashSet<Matchup> = fd_games.iter().map(|g| g.matchup.clone()).collect();

    let res = matchups
        .into_iter()
        .filter_map(|matchup| {
            games
                .iter()
                .find(|g| g.away_team == matchup.away && g.home_team == matchup.home)
                .map(|game| (matchup, Arc::new(game.clone())))
        })
        .collect();

    Ok(res)
}
