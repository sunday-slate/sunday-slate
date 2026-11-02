use crate::salaried_players::{SalariedPlayer, fd_game_map};
use crate::slates::{self, Slate, SlateId};
use crate::store::player_link;
use crate::{NflData, NflDataError, PlayerIdentity, SeasonWeek, scoring};
use fanduel_data::SalariedPlayer as FdSalariedPlayer;
use nfl_model::{DfsPosition, DfsPositionSlotType, Game, NflPlayerId};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

/// Salaried players for one slate, keyed by the roster slots they can fill.
type SalariedPlayersBySlot = HashMap<DfsPositionSlotType, Vec<SalariedPlayer>>;

#[derive(Default)]
struct Snapshot {
    slates: BTreeMap<SlateId, Slate>,
    salaried_players: HashMap<SlateId, SalariedPlayersBySlot>,
}

impl Snapshot {
    async fn build(nfl: &NflData) -> Result<Self, NflDataError> {
        let all_slates: Vec<Slate> = slates::all(nfl).await?.into_iter().collect();

        // stored slates exist for every week, even those without games or
        // salaries; only the ones that resolve to players belong in a snapshot
        let salaried_players: HashMap<SlateId, SalariedPlayersBySlot> =
            build_salaried_players(nfl, all_slates.as_slice())
                .await?
                .into_iter()
                .filter(|(_, by_slot)| !by_slot.is_empty())
                .collect();

        let slates = all_slates
            .into_iter()
            .filter(|slate| salaried_players.contains_key(&slate.id))
            .map(|slate| (slate.id, slate))
            .collect();

        Ok(Self {
            slates,
            salaried_players,
        })
    }

    /// All slates with salaried players in ascending order
    pub fn slates(&self) -> impl Iterator<Item = &Slate> + '_ {
        self.slates.values()
    }

    pub fn salaried_players_for_position(
        &self,
        slate_id: &SlateId,
        pos: &DfsPositionSlotType,
    ) -> &[SalariedPlayer] {
        self.salaried_players
            .get(slate_id)
            .and_then(|by_slot| by_slot.get(pos))
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }
}

/// Salaried players for every given slate, bucketed by the roster slots they
/// can fill.
///
/// This mirrors [`for_position`](crate::salaried_players::for_position) for each
/// slate: map the week's salaries onto the slate's games, resolve FanDuel ids
/// through persisted links, drop anything outside the slate or without an
/// identity, and fill in season-to-date averages. It differs in doing that once
/// per slate for every position instead of once per position, so slates sharing
/// a week share one salary read and one averages pass.
async fn build_salaried_players(
    nfl: &NflData,
    slates: &[Slate],
) -> Result<HashMap<SlateId, SalariedPlayersBySlot>, NflDataError> {
    let weeks: HashSet<SeasonWeek> = slates.iter().map(|slate| slate.season_week).collect();
    let salaries_by_week = salaries_by_week(nfl, weeks).await?;
    let facts = PlayerFacts::load(nfl, &salaries_by_week).await?;

    // each slate needs its own games, so slates are visited one at a time
    let mut by_slate = HashMap::new();
    for slate in slates {
        let players = slate_salaried_players(nfl, slate, &salaries_by_week, &facts).await?;
        by_slate.insert(slate.id, players);
    }
    Ok(by_slate)
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
    facts: &PlayerFacts,
) -> Result<SalariedPlayersBySlot, NflDataError> {
    let Some(salaries) = salaries_by_week
        .get(&slate.season_week)
        .filter(|salaries| !salaries.is_empty())
    else {
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
            facts.salaried_player(salary, averages, game)
        })
        .collect();

    Ok(by_slot(&players))
}

/// Bucket players by the slots they can fill, leaving out the empty buckets.
fn by_slot(players: &[SalariedPlayer]) -> SalariedPlayersBySlot {
    let slots: HashSet<DfsPositionSlotType> = players
        .iter()
        .flat_map(|player| slots_for(player.position))
        .collect();

    slots
        .into_iter()
        .filter_map(|slot| {
            let fillable: Vec<SalariedPlayer> = players
                .iter()
                .filter(|player| slot.allows(player.position))
                .cloned()
                .collect();
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
    std::iter::once(DfsPositionSlotType::Position(position)).chain(flex)
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
        let gsis_ids: Vec<String> = nfl_id_by_fd_id.values().map(|id| id.0.clone()).collect();
        let identities: HashMap<NflPlayerId, PlayerIdentity> = nfl
            .identify(&gsis_ids)
            .await?
            .into_iter()
            .map(|(gsis_id, ident)| (NflPlayerId(gsis_id), ident))
            .collect();

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
            let averages = scoring::season_averages(nfl, week.season, &ids, week.week).await?;
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
        // a D/ST entry has no player: its points come from the team's
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
                name: format!("{} D/ST", salary.team),
                position: salary.position,
                team: salary.team,
                salary: salary.salary,
                games_played,
                fppg,
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
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use fanduel_data::{
        Game as FdGame, Matchup, Player as FdPlayer, SalariedPlayer as FdSalariedPlayer,
    };
    use nfl_model::{DfsPosition, Player, PlayerWeekStats, Season, TeamAbbr, TeamWeekStats};
    use time::OffsetDateTime;
    use time::macros::datetime;

    use super::*;
    use crate::SeasonType;
    use crate::store::player_link::PlayerLink;

    fn week_13() -> SeasonWeek {
        SeasonWeek::new(Season(2026), SeasonType::Reg, 13).expect("week 13")
    }

    fn week_12() -> SeasonWeek {
        SeasonWeek::new(Season(2026), SeasonType::Reg, 12).expect("week 12")
    }

    fn nfl_game(id: &str, kickoff: OffsetDateTime, away: TeamAbbr, home: TeamAbbr) -> Game {
        Game {
            game_id: id.into(),
            season_week: week_13(),
            kickoff: Some(kickoff),
            away_team: away,
            home_team: home,
            home_score: None,
            away_score: None,
        }
    }

    fn nfl_player(gsis_id: &str, name: &str) -> Player {
        Player {
            gsis_id: gsis_id.into(),
            espn_id: None,
            full_name: name.into(),
            first_name: None,
            last_name: None,
            position: Some("RB".into()),
            latest_team: Some(TeamAbbr::GB),
            headshot_url: None,
        }
    }

    fn fd_salary(
        player_id: Option<u32>,
        name: &str,
        team: TeamAbbr,
        position: DfsPosition,
        salary: u32,
        matchup: (TeamAbbr, TeamAbbr),
    ) -> FdSalariedPlayer {
        FdSalariedPlayer {
            player: player_id.map(|id| FdPlayer {
                id,
                first_name: name.into(),
                last_name: String::new(),
            }),
            game: FdGame {
                season_week: week_13(),
                matchup: Matchup {
                    away: matchup.0,
                    home: matchup.1,
                },
            },
            salary,
            position,
            team,
        }
    }

    fn salaries() -> Vec<FdSalariedPlayer> {
        let det_gb = (TeamAbbr::DET, TeamAbbr::GB);
        let kc_buf = (TeamAbbr::KC, TeamAbbr::BUF);
        vec![
            fd_salary(
                Some(101),
                "QB One",
                TeamAbbr::GB,
                DfsPosition::Qb,
                8_000,
                det_gb,
            ),
            fd_salary(
                Some(102),
                "RB One",
                TeamAbbr::GB,
                DfsPosition::Rb,
                9_000,
                det_gb,
            ),
            fd_salary(None, "", TeamAbbr::GB, DfsPosition::Dst, 4_000, det_gb),
            fd_salary(
                Some(103),
                "WR One",
                TeamAbbr::KC,
                DfsPosition::Wr,
                6_000,
                kc_buf,
            ),
            fd_salary(
                Some(104),
                "TE One",
                TeamAbbr::KC,
                DfsPosition::Te,
                5_000,
                kc_buf,
            ),
            fd_salary(None, "", TeamAbbr::BUF, DfsPosition::Dst, 3_500, kc_buf),
            // Not in any slate: Sunday night is past the main slate's window.
            fd_salary(
                Some(105),
                "SNF QB",
                TeamAbbr::SF,
                DfsPosition::Qb,
                7_000,
                (TeamAbbr::SF, TeamAbbr::LA),
            ),
            // On a slate game, but with no persisted link.
            fd_salary(
                Some(106),
                "Unlinked TE",
                TeamAbbr::GB,
                DfsPosition::Te,
                3_000,
                det_gb,
            ),
            // Linked, but the linked player has no identity anywhere.
            fd_salary(
                Some(107),
                "Ghost TE",
                TeamAbbr::GB,
                DfsPosition::Te,
                3_000,
                det_gb,
            ),
        ]
    }

    fn links() -> Vec<PlayerLink> {
        [
            (101, "00-QB"),
            (102, "00-RB"),
            (103, "00-WR"),
            (104, "00-TE"),
            (105, "00-SFQB"),
            (107, "00-GHOST"),
        ]
        .into_iter()
        .map(|(fd_player_id, gsis_player_id)| PlayerLink {
            fd_player_id,
            gsis_player_id: gsis_player_id.into(),
        })
        .collect()
    }

    fn names(players: &[SalariedPlayer]) -> Vec<&str> {
        let mut names: Vec<&str> = players.iter().map(|p| p.name.as_str()).collect();
        names.sort_unstable();
        names
    }

    fn player_line(
        season_week: SeasonWeek,
        gsis_id: &str,
        rushing_yards: i32,
        rushing_tds: u32,
    ) -> PlayerWeekStats {
        PlayerWeekStats {
            season: season_week.season,
            week: season_week.week,
            season_type: season_week.season_type,
            gsis_id: gsis_id.into(),
            team: TeamAbbr::GB,
            opponent: Some(TeamAbbr::DET),
            completions: 0,
            attempts: 0,
            passing_yards: 0,
            passing_tds: 0,
            passing_interceptions: 0,
            rushing_attempts: 0,
            rushing_yards,
            rushing_tds,
            targets: 0,
            receptions: 0,
            receiving_yards: 0,
            receiving_tds: 0,
            fumbles_lost: 0,
            two_point_conversions: 0,
            special_teams_tds: 0,
            fumble_recovery_tds: 0,
        }
    }

    fn team_line(season_week: SeasonWeek, sacks: u32, points_allowed: i32) -> TeamWeekStats {
        TeamWeekStats {
            season: season_week.season,
            week: season_week.week,
            season_type: season_week.season_type,
            team: TeamAbbr::GB,
            opponent: TeamAbbr::DET,
            game_id: "tg".into(),
            sacks,
            interceptions: 0,
            fumble_recoveries: 0,
            safeties: 0,
            touchdowns: 0,
            blocked_kicks: 0,
            conversion_returns: 0,
            points_allowed,
        }
    }

    async fn seeded() -> NflData {
        let nfl = NflData::in_memory().await.unwrap();

        nfl.seed_for_test(
            &[
                nfl_player("00-QB", "QB One"),
                nfl_player("00-RB", "RB One"),
                nfl_player("00-WR", "WR One"),
                nfl_player("00-TE", "TE One"),
                nfl_player("00-SFQB", "SNF QB"),
            ],
            &[
                nfl_game(
                    "tg",
                    datetime!(2026-11-26 17:30 UTC),
                    TeamAbbr::DET,
                    TeamAbbr::GB,
                ),
                nfl_game(
                    "kc",
                    datetime!(2026-11-29 18:00 UTC),
                    TeamAbbr::KC,
                    TeamAbbr::BUF,
                ),
                nfl_game(
                    "snf",
                    datetime!(2026-11-30 01:20 UTC),
                    TeamAbbr::SF,
                    TeamAbbr::LA,
                ),
            ],
        )
        .await
        .unwrap();

        nfl.save_fanduel_salaries(&salaries()).await.unwrap();
        player_link::save_links(&nfl.store, &links()).await.unwrap();
        nfl.seed_week_stats_for_test(
            Season(2026),
            &[
                player_line(week_12(), "00-RB", 50, 1),
                // Week 13 must not leak into week 13's averages.
                player_line(week_13(), "00-RB", 100, 2),
            ],
            &[team_line(week_12(), 2, 20), team_line(week_13(), 10, 0)],
        )
        .await
        .unwrap();

        nfl
    }

    async fn week_13_slates(nfl: &NflData) -> Vec<Slate> {
        nfl.refresh_slates()
            .await
            .unwrap()
            .into_iter()
            .filter(|slate| slate.season_week == week_13())
            .collect()
    }

    fn find<'a>(slates: &'a [Slate], name: &str) -> &'a Slate {
        slates
            .iter()
            .find(|slate| slate.name == name)
            .unwrap_or_else(|| panic!("{name} slate"))
    }

    #[tokio::test]
    async fn buckets_players_by_slot_across_slates() {
        let nfl = seeded().await;
        let all = week_13_slates(&nfl).await;
        let thanksgiving = find(&all, "Thanksgiving");
        let main = find(&all, "Week 13");

        let built = build_salaried_players(&nfl, &all).await.unwrap();
        let tg = &built[&thanksgiving.id];
        let main = &built[&main.id];

        // Thursday is the DET@GB game only, so there is no WR or TE bucket.
        assert_eq!(
            names(&tg[&DfsPositionSlotType::Position(DfsPosition::Qb)]),
            ["QB One"]
        );
        assert_eq!(
            names(&tg[&DfsPositionSlotType::Position(DfsPosition::Rb)]),
            ["RB One"]
        );
        assert_eq!(
            names(&tg[&DfsPositionSlotType::Position(DfsPosition::Dst)]),
            ["GB D/ST"]
        );
        assert_eq!(names(&tg[&DfsPositionSlotType::Flex]), ["RB One"]);
        assert!(!tg.contains_key(&DfsPositionSlotType::Position(DfsPosition::Wr)));
        assert!(!tg.contains_key(&DfsPositionSlotType::Position(DfsPosition::Te)));

        // The main slate holds the Sunday afternoon game only.
        assert_eq!(
            names(&main[&DfsPositionSlotType::Position(DfsPosition::Wr)]),
            ["WR One"]
        );
        assert_eq!(
            names(&main[&DfsPositionSlotType::Position(DfsPosition::Te)]),
            ["TE One"]
        );
        assert_eq!(
            names(&main[&DfsPositionSlotType::Position(DfsPosition::Dst)]),
            ["BUF D/ST"]
        );
        assert_eq!(
            names(&main[&DfsPositionSlotType::Flex]),
            ["TE One", "WR One"]
        );
        assert!(!main.contains_key(&DfsPositionSlotType::Position(DfsPosition::Qb)));
    }

    #[tokio::test]
    async fn fills_salary_game_and_week_to_date_averages() {
        let nfl = seeded().await;
        let all = week_13_slates(&nfl).await;
        let thanksgiving = find(&all, "Thanksgiving");

        let built = build_salaried_players(&nfl, &all).await.unwrap();
        let tg = &built[&thanksgiving.id];

        let rb = &tg[&DfsPositionSlotType::Position(DfsPosition::Rb)][0];
        assert_eq!(rb.name, "RB One");
        assert_eq!(rb.id, Some(NflPlayerId("00-RB".into())));
        assert_eq!(rb.team, TeamAbbr::GB);
        assert_eq!(rb.salary, 9_000);
        assert_eq!(rb.game.game_id, "tg");
        // Week 12 only: 50 rushing yards (5) and 1 rushing TD (6).
        assert_eq!(rb.games_played, 1);
        assert!((rb.fppg - 11.0).abs() < 1e-9);

        let dst = &tg[&DfsPositionSlotType::Position(DfsPosition::Dst)][0];
        assert_eq!(dst.id, None);
        // Week 12 only: 2 sacks (2) and 20 points allowed (1).
        assert_eq!(dst.games_played, 1);
        assert!((dst.fppg - 3.0).abs() < 1e-9);

        // Linked and in the slate, but with no stats yet.
        let qb = &tg[&DfsPositionSlotType::Position(DfsPosition::Qb)][0];
        assert_eq!(qb.games_played, 0);
        assert_eq!(qb.fppg, 0.0);
    }

    #[tokio::test]
    async fn drops_entries_outside_the_slate_or_without_an_identity() {
        let nfl = seeded().await;
        let all = week_13_slates(&nfl).await;

        let built = build_salaried_players(&nfl, &all).await.unwrap();
        let players: Vec<&SalariedPlayer> = built
            .values()
            .flat_map(|buckets| buckets.values().flatten())
            .collect();

        let ids: HashSet<Option<&str>> = players
            .iter()
            .map(|player| player.id.as_ref().map(|id| id.0.as_str()))
            .collect();
        assert!(!ids.contains(&Some("00-SFQB")));
        assert!(!ids.contains(&Some("00-GHOST")));

        let names: Vec<&str> = players.iter().map(|player| player.name.as_str()).collect();
        assert!(!names.contains(&"SNF QB"));
        assert!(!names.contains(&"Unlinked TE"));
        assert!(!names.contains(&"Ghost TE"));
    }

    #[tokio::test]
    async fn unknown_slate_is_an_empty_snapshot_entry() {
        let nfl = seeded().await;
        let unknown = Slate {
            id: SlateId(999_999_999),
            season_week: week_13(),
            name: "Elsewhere".into(),
            kickoff: None,
        };

        let built = build_salaried_players(&nfl, std::slice::from_ref(&unknown))
            .await
            .unwrap();

        assert!(built[&unknown.id].is_empty());
    }

    #[tokio::test]
    async fn snapshot_keeps_only_slates_with_salaried_players() {
        let nfl = seeded().await;
        nfl.refresh_slates().await.unwrap();
        let stored = slates::all(&nfl).await.unwrap();

        let snapshot = Snapshot::build(&nfl).await.unwrap();

        // every week is stored as a slate, but only week 13 has salaries
        assert!(snapshot.slates.len() < stored.len());
        let mut names: Vec<&str> = snapshot
            .slates
            .values()
            .map(|slate| slate.name.as_str())
            .collect();
        names.sort_unstable();
        assert_eq!(names, ["Thanksgiving", "Week 13"]);

        // the kept slates and the player buckets describe the same slates
        let slate_ids: HashSet<SlateId> = snapshot.slates.keys().copied().collect();
        let player_ids: HashSet<SlateId> = snapshot.salaried_players.keys().copied().collect();
        assert_eq!(slate_ids, player_ids);
        assert!(
            snapshot
                .salaried_players
                .values()
                .all(|by_slot| !by_slot.is_empty())
        );
    }

    #[tokio::test]
    async fn no_slates_builds_nothing() {
        let nfl = seeded().await;

        assert!(build_salaried_players(&nfl, &[]).await.unwrap().is_empty());
    }
}
