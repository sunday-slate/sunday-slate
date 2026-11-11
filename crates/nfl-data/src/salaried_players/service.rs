use crate::salaried_players::{SalariedPlayer, ScoredStats, WeeklyStats};
use crate::slates::{self, Slate, SlateId};
use crate::store::player_link;
use crate::{NflData, NflDataError, PlayerIdentity, scoring};
use crate::{PlayerWeekStats, TeamWeekStats};
use fanduel_data::{Matchup, SalariedPlayer as FdSalariedPlayer};
use nfl_model::{DfsPosition, DfsPositionSlotType, Game, NflPlayerId, SeasonWeek, TeamAbbr};
use std::collections::{HashMap, HashSet};
use std::iter;
use std::sync::Arc;

/// Salaried players for one slate, keyed by the roster slots they can fill.
pub(crate) type SalariedPlayersBySlot = HashMap<DfsPositionSlotType, Vec<SalariedPlayer>>;

/// Returns salary buckets and slate IDs with complete team-game coverage.
pub(crate) async fn build_salaried_players(
    nfl: &NflData,
    slates: &[Slate],
    games: &HashMap<SlateId, Vec<Game>>,
) -> Result<(HashMap<SlateId, SalariedPlayersBySlot>, HashSet<SlateId>), NflDataError> {
    let weeks: HashSet<SeasonWeek> = slates.iter().map(|slate| slate.season_week).collect();
    let salaries_by_week = salaries_by_week(nfl, weeks).await?;
    let facts = PlayerFacts::load(nfl, &salaries_by_week).await?;
    let weekly = weekly_inputs(nfl, &salaries_by_week).await?;

    // each slate needs its own games, so slates are visited one at a time
    let mut by_slate = HashMap::new();
    let mut available = HashSet::new();
    for slate in slates {
        let players =
            slate_salaried_players(nfl, slate, &salaries_by_week, &weekly, &facts).await?;
        if !players.is_empty()
            && let Some(inputs) = weekly.get(&slate.season_week)
            && stats_available(inputs, games.get(&slate.id).map(Vec::as_slice))
        {
            available.insert(slate.id);
        }
        by_slate.insert(slate.id, players);
    }
    Ok((by_slate, available))
}

struct WeeklyInputs {
    players: HashMap<NflPlayerId, PlayerWeekStats>,
    defenses: HashMap<TeamAbbr, TeamWeekStats>,
    team_games: HashSet<(String, TeamAbbr)>,
}

async fn weekly_inputs(
    nfl: &NflData,
    salaries_by_week: &HashMap<SeasonWeek, Vec<FdSalariedPlayer>>,
) -> Result<HashMap<SeasonWeek, WeeklyInputs>, NflDataError> {
    let mut by_week = HashMap::new();
    for week in salaries_by_week
        .iter()
        .filter(|(_, salaries)| !salaries.is_empty())
        .map(|(week, _)| *week)
    {
        let players: HashMap<NflPlayerId, PlayerWeekStats> = nfl
            .player_week_stats(week)
            .await?
            .into_iter()
            .filter(|row| row.season_week == week)
            .map(|row| (NflPlayerId(row.gsis_id.clone()), row))
            .collect();
        let teams: Vec<TeamWeekStats> = nfl
            .team_week_stats(week)
            .await?
            .into_iter()
            .filter(|row| row.season_week == week)
            .collect();
        let team_games = teams
            .iter()
            .map(|row| (row.game_id.clone(), row.team))
            .collect();
        let defenses = teams.into_iter().map(|row| (row.team, row)).collect();
        by_week.insert(
            week,
            WeeklyInputs {
                players,
                defenses,
                team_games,
            },
        );
    }
    Ok(by_week)
}

fn stats_available(inputs: &WeeklyInputs, games: Option<&[Game]>) -> bool {
    let Some(games) = games.filter(|games| !games.is_empty()) else {
        return false;
    };
    games.iter().all(|game| {
        [game.home_team, game.away_team]
            .into_iter()
            .all(|team| inputs.team_games.contains(&(game.game_id.clone(), team)))
    })
}

/// One salary read per week, shared by every slate in that week.
async fn salaries_by_week(
    nfl: &NflData,
    weeks: HashSet<SeasonWeek>,
) -> Result<HashMap<SeasonWeek, Vec<FdSalariedPlayer>>, NflDataError> {
    let mut by_week = HashMap::new();
    for week in weeks {
        by_week.insert(week, nfl.fanduel.salaried_players_for(week).await?);
    }
    Ok(by_week)
}

/// Every salaried player inside `slate`'s games, bucketed by the slots they
/// fill. A slate whose week has no salaries, or that matches no games, gets an
/// empty entry.
async fn slate_salaried_players(
    nfl: &NflData,
    slate: &Slate,
    salaries_by_week: &HashMap<SeasonWeek, Vec<FdSalariedPlayer>>,
    weekly: &HashMap<SeasonWeek, WeeklyInputs>,
    facts: &PlayerFacts,
) -> Result<SalariedPlayersBySlot, NflDataError> {
    let Some(salaries) = salaries_by_week
        .get(&slate.season_week)
        .filter(|salaries| !salaries.is_empty())
    else {
        return Ok(SalariedPlayersBySlot::new());
    };
    let Some(inputs) = weekly.get(&slate.season_week) else {
        return Ok(SalariedPlayersBySlot::new());
    };

    // remove any fd players not in the slate
    let fd_games: Vec<fanduel_data::Game> =
        salaries.iter().map(|salary| salary.game.clone()).collect();
    let games = fd_game_map(nfl, &slate.id, &fd_games).await?;

    // every week with salaries got an averages pass in `PlayerFacts::load`
    let averages = &facts.averages_by_week[&slate.season_week];
    let players: Vec<SalariedPlayer> = salaries
        .iter()
        .filter_map(|salary| {
            let game = games.get(&salary.game.matchup)?;
            let mut player = facts.salaried_player(salary, averages, game)?;
            player.scored_stats = match &player.id {
                Some(id) => inputs.players.get(id).map(|row| ScoredStats {
                    stats: WeeklyStats::Player(row.clone()),
                    score: scoring::score_player(row),
                }),
                None => inputs.defenses.get(&player.team).map(|row| ScoredStats {
                    stats: WeeklyStats::Defense(row.clone()),
                    score: scoring::score_defense(row),
                }),
            };
            Some(player)
        })
        .collect();

    Ok(by_slot(&players))
}

/// Bucket players by the slots they can fill, leaving out the empty buckets.
/// Each bucket is ordered salary-descending for display.
fn by_slot(players: &[SalariedPlayer]) -> SalariedPlayersBySlot {
    let slots: HashSet<DfsPositionSlotType> = players
        .iter()
        .flat_map(|player| slots_for(player.position))
        .collect();

    slots
        .into_iter()
        .filter_map(|slot| {
            let mut fillable: Vec<SalariedPlayer> = players
                .iter()
                .filter(|player| slot.allows(player.position))
                .cloned()
                .collect();
            fillable.sort_by(|a, b| b.salary.cmp(&a.salary).then_with(|| a.name.cmp(&b.name)));
            (!fillable.is_empty()).then_some((slot, fillable))
        })
        .collect()
}

/// The slots a player at `position` can fill: its own position, plus flex when
/// the position is eligible.
fn slots_for(position: DfsPosition) -> impl Iterator<Item = DfsPositionSlotType> {
    let flex = DfsPositionSlotType::Flex
        .allows(position)
        .then_some(DfsPositionSlotType::Flex);
    iter::once(DfsPositionSlotType::Position(position)).chain(flex)
}

/// The lookups every slate needs: FanDuel ids to NFL ids, NFL ids to display
/// facts, and the week's season-to-date averages.
struct PlayerFacts {
    nfl_id_by_fd_id: HashMap<u32, NflPlayerId>,
    identities: HashMap<NflPlayerId, PlayerIdentity>,
    averages_by_week: HashMap<SeasonWeek, scoring::SeasonAverages>,
}

impl PlayerFacts {
    /// Resolve every salary in `salaries_by_week` through persisted links and
    /// nflverse identities, then take one averages pass per week.
    async fn load(
        nfl: &NflData,
        salaries_by_week: &HashMap<SeasonWeek, Vec<FdSalariedPlayer>>,
    ) -> Result<Self, NflDataError> {
        // map FanDuel IDs to Nflverse gsis IDs
        let fd_ids: Vec<u32> = salaries_by_week
            .values()
            .flatten()
            .filter_map(|salary| salary.player.as_ref().map(|player| player.id))
            .collect();
        let nfl_id_by_fd_id: HashMap<u32, NflPlayerId> =
            player_link::links_for_fd_ids(nfl.store.reader(), &fd_ids)
                .await?
                .into_iter()
                .map(|(fd_id, gsis_id)| (fd_id, NflPlayerId(gsis_id)))
                .collect();

        // then gsis IDs to display facts
        let gsis_ids: Vec<NflPlayerId> = nfl_id_by_fd_id.values().cloned().collect();
        let identities = nfl.identify(&gsis_ids).await?;

        // averages are week-scoped, so one pass per week covers all of its slates
        let mut averages_by_week = HashMap::new();
        for (week, salaries) in salaries_by_week
            .iter()
            .filter(|(_, salaries)| !salaries.is_empty())
        {
            let ids: Vec<String> = salaries
                .iter()
                .filter_map(|salary| salary.player.as_ref())
                .filter_map(|player| nfl_id_by_fd_id.get(&player.id))
                .map(|id| id.0.clone())
                .collect();
            let averages = scoring::season_averages(nfl, *week, &ids).await?;
            averages_by_week.insert(*week, averages);
        }

        Ok(Self {
            nfl_id_by_fd_id,
            identities,
            averages_by_week,
        })
    }

    /// One FanDuel entry with its identity and averages, or `None` when it
    /// cannot be resolved the way `for_position` resolves it.
    fn salaried_player(
        &self,
        salary: &FdSalariedPlayer,
        averages: &scoring::SeasonAverages,
        game: &Arc<Game>,
    ) -> Option<SalariedPlayer> {
        // a DST entry has no player: its points come from the team's
        // defensive line, keyed by team rather than gsis id
        if salary.position.is_defense() {
            let (games_played, fppg) = averages
                .dsts
                .get(&salary.team)
                .map(|ppg| (ppg.games_played, ppg.avg_points))
                .unwrap_or_default();

            return Some(SalariedPlayer {
                id: None,
                game: Arc::clone(game),
                name: format!("{} DST", salary.team),
                position: salary.position,
                team: salary.team,
                salary: salary.salary,
                games_played,
                fppg,
                scored_stats: None,
            });
        }

        let fd_player_id = salary.player.as_ref().map(|fd_player| fd_player.id)?;
        let nfl_player_id = self.nfl_id_by_fd_id.get(&fd_player_id)?;
        let identity = self.identities.get(nfl_player_id)?;
        let (games_played, fppg) = averages
            .players
            .get(nfl_player_id)
            .map(|ppg| (ppg.games_played, ppg.avg_points))
            .unwrap_or_default();

        Some(SalariedPlayer {
            id: Some(nfl_player_id.clone()),
            game: Arc::clone(game),
            name: identity.name.clone(),
            position: salary.position,
            team: salary.team,
            salary: salary.salary,
            games_played,
            fppg,
            scored_stats: None,
        })
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
