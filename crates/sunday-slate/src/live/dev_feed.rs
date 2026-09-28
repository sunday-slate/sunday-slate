//! Dev-only live feed: ramps each slate game's nflverse final line from zero to
//! the real box score so a personal dev-data projection can render live
//! feedback without a provider key, network, or database writes.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;

use nfl_data::{
    EspnPlayerId, Game, LiveGame, LiveGamePhase, LiveGameSnapshot, LivePlayerStats, LiveTeamStats,
    ProviderOutcome, ProviderResponse, Season,
};
use tank01_data::{PollEvent, PollRequest};
use time::OffsetDateTime;

use crate::contests::service::slate_locked;
use crate::contests::{ContestId, store};
use crate::live::identity::LiveIdentityIndex;
use crate::live::snapshot::live_game;
use crate::{AppError, AppState};

const STARTUP_RECOVERY: ::time::Duration = ::time::Duration::hours(12);

pub(crate) const DEV_TICKS: u64 = 40;
/// The net drop, in yards, the corrected field takes at its correction tick.
/// Sized above one tick of ramp gain so the dip survives into the displayed
/// points score; the field recovers to its true final line by tick 40.
pub(crate) const CORRECTION_PASSING_YARDS: i32 = -40;
pub(crate) const CORRECTION_RUSHING_YARDS: i32 = -20;
pub(crate) const PASSING_CORRECTION_TICK: u64 = 20;
pub(crate) const RUSHING_CORRECTION_TICK: u64 = 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RampField {
    PassingYards,
    RushingYards,
}

#[derive(Debug, Clone)]
pub(crate) struct RampPlayer {
    identity: LivePlayerStats,
    finals: LivePlayerStats,
}

#[derive(Debug, Clone)]
pub(crate) struct Ramp {
    game: LiveGame,
    players: Vec<RampPlayer>,
    defenses: Vec<LiveTeamStats>,
    home_score: i32,
    away_score: i32,
    corrections: Vec<(usize, RampField, i32, u64)>,
}

impl Ramp {
    pub(crate) fn new(
        game: LiveGame,
        players: Vec<RampPlayer>,
        defenses: Vec<LiveTeamStats>,
        home_score: i32,
        away_score: i32,
    ) -> Self {
        let mut corrections = Vec::new();
        let passing = players
            .iter()
            .enumerate()
            .max_by_key(|(index, player)| (player.finals.passing_yards, std::cmp::Reverse(*index)));
        if let Some((index, player)) = passing
            && player.finals.passing_yards > 0
        {
            corrections.push((
                index,
                RampField::PassingYards,
                CORRECTION_PASSING_YARDS,
                PASSING_CORRECTION_TICK,
            ));
        }
        let rushing = players
            .iter()
            .enumerate()
            .max_by_key(|(index, player)| (player.finals.rushing_yards, std::cmp::Reverse(*index)));
        if let Some((index, player)) = rushing
            && player.finals.rushing_yards > 0
        {
            corrections.push((
                index,
                RampField::RushingYards,
                CORRECTION_RUSHING_YARDS,
                RUSHING_CORRECTION_TICK,
            ));
        }
        Self {
            game,
            players,
            defenses,
            home_score,
            away_score,
            corrections,
        }
    }

    pub(crate) fn snapshot_at(&self, tick: u64, observed_at: OffsetDateTime) -> LiveGameSnapshot {
        let tick = tick.min(DEV_TICKS);
        let players = self
            .players
            .iter()
            .enumerate()
            .map(|(index, player)| {
                scaled_player(
                    &player.identity,
                    &player.finals,
                    &self.corrections,
                    index,
                    tick,
                )
            })
            .collect::<Vec<_>>();
        let defenses = self
            .defenses
            .iter()
            .map(|defense| scaled_defense(defense, tick))
            .collect::<Vec<_>>();
        let (phase, period, clock) = game_state(tick);
        LiveGameSnapshot {
            game: self.game.clone(),
            observed_at,
            phase,
            period,
            clock,
            home_score: Some(self.home_score),
            away_score: Some(self.away_score),
            players: Some(players),
            defenses: Some(defenses),
        }
    }
}

fn scaled_u32(final_value: u32, tick: u64) -> u32 {
    (u64::from(final_value) * tick / DEV_TICKS) as u32
}

fn scaled_i32(final_value: i32, tick: u64) -> i32 {
    (i64::from(final_value) * tick as i64 / DEV_TICKS as i64) as i32
}

fn game_state(tick: u64) -> (LiveGamePhase, Option<String>, Option<String>) {
    if tick >= DEV_TICKS {
        return (
            LiveGamePhase::Final,
            Some("4".to_string()),
            Some("00:00".to_string()),
        );
    }
    let elapsed = tick * 3600 / DEV_TICKS;
    let period = (elapsed / 900).min(3) + 1;
    let remaining = 900 - (elapsed % 900);
    (
        LiveGamePhase::InProgress,
        Some(period.to_string()),
        Some(format!("{:02}:{:02}", remaining / 60, remaining % 60)),
    )
}

fn corrected_value(final_value: i32, tick: u64, from_tick: u64, correction: i32) -> i32 {
    let scaled = |t: u64| scaled_i32(final_value, t);
    if tick < from_tick {
        return scaled(tick);
    }
    let dipped = scaled(from_tick.saturating_sub(1)) + correction;
    let target = final_value;
    if tick >= DEV_TICKS {
        return target;
    }
    let span = (DEV_TICKS - from_tick) as i64;
    let steps = (tick - from_tick) as i64;
    (i64::from(dipped) + (i64::from(target) - i64::from(dipped)) * steps / span) as i32
}

fn scaled_player(
    identity: &LivePlayerStats,
    finals: &LivePlayerStats,
    corrections: &[(usize, RampField, i32, u64)],
    index: usize,
    tick: u64,
) -> LivePlayerStats {
    let field_value = |field: RampField, final_value: i32| -> i32 {
        match corrections
            .iter()
            .find(|(candidate, candidate_field, _, _)| {
                *candidate == index && *candidate_field == field
            }) {
            Some((_, _, correction, from_tick)) => {
                corrected_value(final_value, tick, *from_tick, *correction)
            }
            None => scaled_i32(final_value, tick),
        }
    };
    LivePlayerStats {
        espn_id: identity.espn_id.clone(),
        name: identity.name.clone(),
        team: identity.team.clone(),
        position: identity.position.clone(),
        opponent: identity.opponent.clone(),
        completions: scaled_u32(finals.completions, tick),
        attempts: scaled_u32(finals.attempts, tick),
        passing_tds: scaled_u32(finals.passing_tds, tick),
        passing_interceptions: scaled_u32(finals.passing_interceptions, tick),
        rushing_attempts: scaled_u32(finals.rushing_attempts, tick),
        rushing_tds: scaled_u32(finals.rushing_tds, tick),
        targets: scaled_u32(finals.targets, tick),
        receptions: scaled_u32(finals.receptions, tick),
        receiving_tds: scaled_u32(finals.receiving_tds, tick),
        fumbles_lost: scaled_u32(finals.fumbles_lost, tick),
        two_point_conversions: scaled_u32(finals.two_point_conversions, tick),
        special_teams_tds: scaled_u32(finals.special_teams_tds, tick),
        fumble_recovery_tds: scaled_u32(finals.fumble_recovery_tds, tick),
        passing_yards: field_value(RampField::PassingYards, finals.passing_yards).max(0),
        rushing_yards: field_value(RampField::RushingYards, finals.rushing_yards).max(0),
        receiving_yards: scaled_i32(finals.receiving_yards, tick),
    }
}

fn scaled_defense(defense: &LiveTeamStats, tick: u64) -> LiveTeamStats {
    LiveTeamStats {
        team: defense.team.clone(),
        opponent: defense.opponent.clone(),
        sacks: scaled_u32(defense.sacks, tick),
        interceptions: scaled_u32(defense.interceptions, tick),
        fumble_recoveries: scaled_u32(defense.fumble_recoveries, tick),
        safeties: scaled_u32(defense.safeties, tick),
        touchdowns: scaled_u32(defense.touchdowns, tick),
        blocked_kicks: scaled_u32(defense.blocked_kicks, tick),
        conversion_returns: scaled_u32(defense.conversion_returns, tick),
        points_allowed: scaled_i32(defense.points_allowed, tick),
        dst_present: true,
        team_stats_present: true,
    }
}

fn identity_of(finals: &LivePlayerStats) -> LivePlayerStats {
    LivePlayerStats {
        espn_id: finals.espn_id.clone(),
        name: finals.name.clone(),
        team: finals.team.clone(),
        position: finals.position.clone(),
        opponent: finals.opponent.clone(),
        completions: 0,
        attempts: 0,
        passing_tds: 0,
        passing_interceptions: 0,
        rushing_attempts: 0,
        rushing_tds: 0,
        targets: 0,
        receptions: 0,
        receiving_tds: 0,
        fumbles_lost: 0,
        two_point_conversions: 0,
        special_teams_tds: 0,
        fumble_recovery_tds: 0,
        passing_yards: 0,
        rushing_yards: 0,
        receiving_yards: 0,
    }
}

fn resolve_games(ids: &[crate::contests::NflGameId], schedule: &[Game]) -> Vec<Game> {
    ids.iter()
        .filter_map(|id| {
            schedule
                .iter()
                .find(|game| game.gsis_game_id == id.0)
                .cloned()
        })
        .collect()
}

/// Real-game wall span the feed treats as "in progress at now".
const GAME_LENGTH: ::time::Duration = ::time::Duration::hours(4);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Lane {
    NotStarted,
    InFlight,
    EndedBeforeNow,
}

fn lane(game: &Game, now: OffsetDateTime) -> Lane {
    let Some(kickoff) = game.kickoff_eastern() else {
        return Lane::InFlight;
    };
    if now - kickoff >= GAME_LENGTH {
        Lane::EndedBeforeNow
    } else if kickoff > now {
        Lane::NotStarted
    } else {
        Lane::InFlight
    }
}

pub(crate) async fn run(state: AppState) {
    if let Err(error) = run_inner(&state).await {
        tracing::error!(error = ?error, "live dev feed failed");
    }
}

async fn run_inner(state: &AppState) -> Result<(), AppError> {
    let now = state.now_eastern();
    let schedule = state.nfl.games(Season(state.config.season)).await?;
    let materialized = store::materialized_games(state.db.reader()).await?;

    let mut targets: Vec<(ContestId, Vec<Game>, Vec<LiveGame>)> = Vec::new();
    let mut distinct: BTreeMap<String, Game> = BTreeMap::new();
    for (contest, ids) in &materialized {
        let games = resolve_games(ids, &schedule);
        if games.is_empty() || games.len() != ids.len() {
            continue;
        }
        if !slate_locked(&games, now) {
            continue;
        }
        let Some(last) = games.iter().filter_map(Game::kickoff_eastern).max() else {
            continue;
        };
        if last < now - STARTUP_RECOVERY {
            continue;
        }
        if super::slate_tuple(&games).is_none() {
            continue;
        }
        let live_games = games.iter().filter_map(live_game).collect::<Vec<_>>();
        if live_games.is_empty() {
            continue;
        }
        for game in &games {
            distinct
                .entry(game.gsis_game_id.clone())
                .or_insert_with(|| game.clone());
        }
        targets.push((*contest, games, live_games));
    }

    if targets.is_empty() {
        tracing::info!("live dev feed: no live contests");
        return Ok(());
    }

    for (contest, games, _) in &targets {
        let (season, week, _) = super::slate_tuple(games).expect("uniform slate");
        let mut teams = games
            .iter()
            .flat_map(|game| [game.home_team.clone(), game.away_team.clone()])
            .collect::<Vec<_>>();
        teams.sort_by(|a, b| a.0.cmp(&b.0));
        teams.dedup_by(|a, b| a.0 == b.0);
        let identity = Arc::new(LiveIdentityIndex::build(&state.nfl, season, week, &teams).await?);
        let key = super::slate_tuple(games).expect("uniform slate");
        state.injuries.publish(
            key,
            crate::injuries::SlateInjuries {
                by_gsis: crate::injuries::synthetic_dev(identity.player_ids()),
                report_date: None,
                attempted_at: Some(now),
            },
        );
        state.live.notify(*contest);
        state
            .live
            .rebase_contest(*contest, games.clone(), identity, &[], now);
    }

    let mut ramps = BTreeMap::new();
    for (gsis_game_id, game) in &distinct {
        if let Some(ramp) = plan_ramp(state, game).await? {
            ramps.insert(gsis_game_id.clone(), (lane(game, now), ramp));
        }
    }

    for (contest, _, live_games) in &targets {
        for game in live_games {
            let Some((Lane::EndedBeforeNow, ramp)) = ramps.get(&game.gsis_game_id) else {
                continue;
            };
            let event = event(game, ramp.snapshot_at(DEV_TICKS, now), DEV_TICKS);
            state.live.publish_event(*contest, &event, live_games);
        }
    }

    let tick_ms = Duration::from_millis(state.config.live_dev_feed_tick_ms.max(1));
    let mut shutdown = state.live.shutdown_receiver();
    for tick in 0..=DEV_TICKS {
        for (contest, _, live_games) in &targets {
            for game in live_games {
                let Some((Lane::InFlight, ramp)) = ramps.get(&game.gsis_game_id) else {
                    continue;
                };
                let event = event(game, ramp.snapshot_at(tick, now), tick);
                state.live.publish_event(*contest, &event, live_games);
            }
        }
        tracing::debug!(tick, "live dev feed tick");
        tokio::select! {
            _ = tokio::time::sleep(tick_ms) => {}
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break;
                }
            }
        }
    }

    tracing::info!("live dev feed: final snapshot published");
    Ok(())
}

async fn plan_ramp(state: &AppState, game: &Game) -> Result<Option<Ramp>, AppError> {
    let Some(live) = live_game(game) else {
        tracing::warn!(game = %game.gsis_game_id, "live dev feed: game has no provider id");
        return Ok(None);
    };
    let season = game.season;
    let week = game.week;
    let home = game.home_team.clone();
    let away = game.away_team.clone();

    let mut rosters = HashMap::new();
    for team in [&home, &away] {
        for entry in state.nfl.weekly_roster(season, week, team).await? {
            if let Some(gsis_id) = entry.gsis_id.as_deref().filter(|id| !id.is_empty()) {
                rosters.insert(gsis_id.to_owned(), entry);
            }
        }
    }

    let mut players = Vec::new();
    for row in state.nfl.player_week_stats(season, week).await? {
        if row.season_type != game.season_type {
            continue;
        }
        let matches_home = row.team == home && row.opponent.as_ref() == Some(&away);
        let matches_away = row.team == away && row.opponent.as_ref() == Some(&home);
        if !matches_home && !matches_away {
            continue;
        }
        let roster = rosters.get(&row.gsis_id);
        let finals = LivePlayerStats {
            espn_id: roster
                .and_then(|entry| entry.espn_id.clone())
                .map(EspnPlayerId),
            name: roster.map(|entry| entry.full_name.clone()),
            team: Some(row.team.clone()),
            position: roster.and_then(|entry| entry.position.clone()),
            opponent: row.opponent.clone(),
            completions: row.completions,
            attempts: row.attempts,
            passing_tds: row.passing_tds,
            passing_interceptions: row.passing_interceptions,
            rushing_attempts: row.rushing_attempts,
            rushing_tds: row.rushing_tds,
            targets: row.targets,
            receptions: row.receptions,
            receiving_tds: row.receiving_tds,
            fumbles_lost: row.fumbles_lost,
            two_point_conversions: row.two_point_conversions,
            special_teams_tds: row.special_teams_tds,
            fumble_recovery_tds: row.fumble_recovery_tds,
            passing_yards: row.passing_yards,
            rushing_yards: row.rushing_yards,
            receiving_yards: row.receiving_yards,
        };
        players.push(RampPlayer {
            identity: identity_of(&finals),
            finals,
        });
    }

    let mut defenses = Vec::new();
    for row in state.nfl.team_week_stats(season, week).await? {
        if row.gsis_game_id != game.gsis_game_id || row.season_type != game.season_type {
            continue;
        }
        defenses.push(LiveTeamStats {
            team: row.team.clone(),
            opponent: row.opponent.clone(),
            sacks: row.sacks,
            interceptions: row.interceptions,
            fumble_recoveries: row.fumble_recoveries,
            safeties: row.safeties,
            touchdowns: row.touchdowns,
            blocked_kicks: row.blocked_kicks,
            conversion_returns: row.conversion_returns,
            points_allowed: row.points_allowed,
            dst_present: true,
            team_stats_present: true,
        });
    }

    if players.is_empty() || defenses.is_empty() {
        tracing::warn!(
            game = %game.gsis_game_id,
            "live dev feed: no nflverse stats for game"
        );
        return Ok(None);
    }

    let home_score = defenses
        .iter()
        .find(|defense| defense.team == away)
        .map_or(0, |defense| defense.points_allowed);
    let away_score = defenses
        .iter()
        .find(|defense| defense.team == home)
        .map_or(0, |defense| defense.points_allowed);

    Ok(Some(Ramp::new(
        live, players, defenses, home_score, away_score,
    )))
}

fn event(game: &LiveGame, snapshot: LiveGameSnapshot, tick: u64) -> PollEvent {
    let now = snapshot.observed_at;
    PollEvent::BoxScore {
        sequence: tick,
        game: game.clone(),
        request: PollRequest {
            endpoint: "getNFLBoxScore".into(),
            query: BTreeMap::from([(String::from("gameID"), game.provider_game_id.clone())]),
        },
        play_by_play: false,
        changed: true,
        response: ProviderResponse {
            requested_at: now,
            received_at: now,
            elapsed_ms: 0,
            http_status: Some(200),
            headers: BTreeMap::new(),
            raw_body: None,
            outcome: ProviderOutcome::Value(snapshot),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::factories;
    use nfl_data::TeamAbbr;

    fn finals(
        team: &str,
        opponent: &str,
        passing_yards: i32,
        rushing_yards: i32,
    ) -> LivePlayerStats {
        LivePlayerStats {
            espn_id: Some(EspnPlayerId(format!("espn-{team}"))),
            name: Some(format!("Player {team}")),
            team: Some(TeamAbbr(team.into())),
            position: Some("QB".into()),
            opponent: Some(TeamAbbr(opponent.into())),
            completions: 20,
            attempts: 30,
            passing_tds: 2,
            passing_interceptions: 1,
            rushing_attempts: 5,
            rushing_tds: 1,
            targets: 0,
            receptions: 0,
            receiving_tds: 0,
            fumbles_lost: 1,
            two_point_conversions: 0,
            special_teams_tds: 0,
            fumble_recovery_tds: 0,
            passing_yards,
            rushing_yards,
            receiving_yards: 0,
        }
    }

    fn ramp() -> Ramp {
        let game = factories::game_at("2025_01_BUF_KC", 1, OffsetDateTime::UNIX_EPOCH);
        let game = live_game(&game).expect("provider id");
        let home = finals("KC", "BUF", 300, 40);
        let away = finals("BUF", "KC", 250, 60);
        let players = vec![
            RampPlayer {
                identity: identity_of(&home),
                finals: home,
            },
            RampPlayer {
                identity: identity_of(&away),
                finals: away,
            },
        ];
        let defenses = vec![
            LiveTeamStats {
                team: TeamAbbr("KC".into()),
                opponent: TeamAbbr("BUF".into()),
                sacks: 2,
                interceptions: 1,
                fumble_recoveries: 0,
                safeties: 0,
                touchdowns: 0,
                blocked_kicks: 0,
                conversion_returns: 0,
                points_allowed: 17,
                dst_present: true,
                team_stats_present: true,
            },
            LiveTeamStats {
                team: TeamAbbr("BUF".into()),
                opponent: TeamAbbr("KC".into()),
                sacks: 1,
                interceptions: 0,
                fumble_recoveries: 1,
                safeties: 0,
                touchdowns: 0,
                blocked_kicks: 0,
                conversion_returns: 0,
                points_allowed: 24,
                dst_present: true,
                team_stats_present: true,
            },
        ];
        Ramp::new(game, players, defenses, 17, 24)
    }

    fn passing(snapshot: &LiveGameSnapshot, index: usize) -> i32 {
        snapshot.players.as_ref().expect("players")[index].passing_yards
    }

    fn rushing(snapshot: &LiveGameSnapshot, index: usize) -> i32 {
        snapshot.players.as_ref().expect("players")[index].rushing_yards
    }

    fn all_stat_fields_zero(player: &LivePlayerStats) -> bool {
        player.completions == 0
            && player.attempts == 0
            && player.passing_tds == 0
            && player.passing_interceptions == 0
            && player.rushing_attempts == 0
            && player.rushing_tds == 0
            && player.targets == 0
            && player.receptions == 0
            && player.receiving_tds == 0
            && player.fumbles_lost == 0
            && player.two_point_conversions == 0
            && player.special_teams_tds == 0
            && player.fumble_recovery_tds == 0
            && player.passing_yards == 0
            && player.rushing_yards == 0
            && player.receiving_yards == 0
    }

    #[test]
    fn ramp_starts_at_kickoff_and_ends_at_finals() {
        let ramp = ramp();
        let start = ramp.snapshot_at(0, OffsetDateTime::UNIX_EPOCH);
        assert_eq!(start.phase, LiveGamePhase::InProgress);
        assert_eq!(start.period.as_deref(), Some("1"));
        assert_eq!(start.clock.as_deref(), Some("15:00"));
        for player in start.players.as_ref().expect("players") {
            assert!(
                all_stat_fields_zero(player),
                "tick 0 is scoreless: {player:?}"
            );
            assert!(player.espn_id.is_some());
            assert!(player.name.is_some());
            assert!(player.team.is_some());
            assert!(player.position.is_some());
            assert!(player.opponent.is_some());
        }
        for defense in start.defenses.as_ref().expect("defenses") {
            assert_eq!(defense.points_allowed, 0);
            assert!(defense.dst_present && defense.team_stats_present);
        }

        let end = ramp.snapshot_at(DEV_TICKS, OffsetDateTime::UNIX_EPOCH);
        assert_eq!(end.phase, LiveGamePhase::Final);
        assert_eq!(end.period.as_deref(), Some("4"));
        assert_eq!(end.clock.as_deref(), Some("00:00"));
        assert_eq!(passing(&end, 0), 300);
        assert_eq!(passing(&end, 1), 250);
        assert_eq!(rushing(&end, 0), 40);
        assert_eq!(rushing(&end, 1), 60);
        let last = &end.players.as_ref().expect("players")[0];
        assert_eq!(last.completions, 20);
        assert_eq!(last.attempts, 30);
        assert_eq!(last.passing_tds, 2);
        assert_eq!(last.passing_interceptions, 1);
        assert_eq!(last.rushing_attempts, 5);
        assert_eq!(last.rushing_tds, 1);
        assert_eq!(last.fumbles_lost, 1);
        assert_eq!(
            last.espn_id.as_ref().map(|id| id.0.as_str()),
            Some("espn-KC")
        );
    }

    #[test]
    fn ramp_dips_only_at_correction_ticks() {
        let ramp = ramp();
        let mut previous = ramp.snapshot_at(0, OffsetDateTime::UNIX_EPOCH);
        for tick in 1..=DEV_TICKS {
            let current = ramp.snapshot_at(tick, OffsetDateTime::UNIX_EPOCH);
            for (index, field) in [(0, RampField::PassingYards), (1, RampField::RushingYards)] {
                let before = match field {
                    RampField::PassingYards => passing(&previous, index),
                    RampField::RushingYards => rushing(&previous, index),
                };
                let after = match field {
                    RampField::PassingYards => passing(&current, index),
                    RampField::RushingYards => rushing(&current, index),
                };
                let correction = match field {
                    RampField::PassingYards => CORRECTION_PASSING_YARDS,
                    RampField::RushingYards => CORRECTION_RUSHING_YARDS,
                };
                let expected_correction_tick = match field {
                    RampField::PassingYards => PASSING_CORRECTION_TICK,
                    RampField::RushingYards => RUSHING_CORRECTION_TICK,
                };
                if tick == expected_correction_tick {
                    assert_eq!(
                        after - before,
                        correction,
                        "tick {tick} drops {field:?} by the scripted correction"
                    );
                } else {
                    assert!(
                        after >= before,
                        "tick {tick} keeps {field:?} non-decreasing (was {before}, now {after})"
                    );
                }
            }
            previous = current;
        }
    }

    #[test]
    fn ramp_stays_within_four_quarters() {
        let ramp = ramp();
        for tick in 0..=DEV_TICKS {
            let snapshot = ramp.snapshot_at(tick, OffsetDateTime::UNIX_EPOCH);
            let period = snapshot.period.clone().expect("period");
            let clock = snapshot.clock.clone().expect("clock");
            assert!(period.as_str() <= "4", "tick {tick} period {period}");
            assert!(clock.as_str() <= "15:00", "tick {tick} clock {clock}");
        }
    }

    #[test]
    fn lane_classifies_against_the_pinned_now() {
        let now = OffsetDateTime::UNIX_EPOCH;
        let at = |offset: ::time::Duration| factories::game_at("2025_01_BUF_KC", 1, now + offset);
        assert_eq!(
            lane(&at(::time::Duration::ZERO), now),
            Lane::InFlight,
            "kickoff == now counts as in flight, matching slate_locked"
        );
        assert_eq!(lane(&at(-::time::Duration::hours(1)), now), Lane::InFlight);
        assert_eq!(
            lane(&at(-(GAME_LENGTH - ::time::Duration::seconds(1))), now),
            Lane::InFlight,
            "games 3h59m past kickoff still ramp"
        );
        assert_eq!(
            lane(&at(-GAME_LENGTH), now),
            Lane::EndedBeforeNow,
            "a game spanning the full wall length is final at now"
        );
        assert_eq!(
            lane(&at(::time::Duration::seconds(1)), now),
            Lane::NotStarted
        );
    }

    #[sqlx::test]
    async fn dev_feed_ramps_and_dips_at_the_correction_tick(pool: sqlx::SqlitePool) {
        use crate::contests::ContestId;
        use crate::entries::NflPlayerId;
        use crate::tests::TestApp;
        use nfl_data::{PlayerWeekStats, Season, TeamWeekStats, WeeklyRosterEntry};

        let kickoff = time::macros::datetime!(2025-09-07 17:00 UTC);
        let game = factories::game_at("2025_01_BUF_KC", 1, kickoff);
        let app = TestApp::from_pool_at_with_live_dev_feed(pool.clone(), kickoff, true).await;

        let mut qb = factories::player("00-A", "Alpha Runner");
        qb.espn_id = Some("live-alpha".into());
        app.nfl
            .seed_for_test(&[qb], std::slice::from_ref(&game))
            .await
            .unwrap();
        app.nfl
            .seed_weekly_roster_for_test(&[WeeklyRosterEntry {
                espn_id: Some("live-alpha".into()),
                position: Some("QB".into()),
                ..factories::weekly_roster_entry("00-A", "Alpha Runner", 1)
            }])
            .await
            .unwrap();

        let qb_stats = PlayerWeekStats {
            gsis_id: "00-A".into(),
            team: TeamAbbr("KC".into()),
            opponent: Some(TeamAbbr("BUF".into())),
            passing_yards: 400,
            ..factories::player_stats("00-A", 1)
        };
        let kc_def = TeamWeekStats {
            gsis_game_id: "2025_01_BUF_KC".into(),
            ..factories::defense_stats("KC", "BUF", 1)
        };
        let buf_def = TeamWeekStats {
            gsis_game_id: "2025_01_BUF_KC".into(),
            ..factories::defense_stats("BUF", "KC", 1)
        };
        app.nfl
            .seed_week_stats_for_test(Season(2025), &[qb_stats], &[kc_def, buf_def])
            .await
            .unwrap();

        let contest = factories::contest_with_games(&pool, "Week 1", &["2025_01_BUF_KC"]).await;
        let contest_id = ContestId(contest);

        let mut config = (*app.state.config).clone();
        config.live_dev_feed_tick_ms = 1;
        let state = AppState {
            config: Arc::new(config),
            ..app.state.clone()
        };

        run(state.clone()).await;

        let snapshot = state
            .live
            .snapshot(contest_id)
            .expect("feed published a snapshot");
        assert_eq!(snapshot.phase(), Some(LiveGamePhase::Final));
        assert_eq!(
            snapshot
                .game("2025_01_BUF_KC")
                .expect("game state in snapshot")
                .player_observed_at,
            Some(kickoff),
            "events carry the pinned clock so staleness math works"
        );
        let qb = snapshot
            .stats
            .players
            .get(&NflPlayerId("00-A".into()))
            .expect("qb resolved into the snapshot");
        assert_eq!(qb.passing_yards, 400);

        let ramp = plan_ramp(&state, &game).await.unwrap().expect("ramp");
        let live_game = live_game(&game).unwrap();
        let points = |state: &AppState| {
            let snapshot = state.live.snapshot(contest_id).expect("snapshot");
            let qb = snapshot
                .stats
                .players
                .get(&NflPlayerId("00-A".into()))
                .expect("qb");
            crate::scoring::score_player(qb).total
        };
        let publish = |tick: u64| {
            let event = event(&live_game, ramp.snapshot_at(tick, kickoff), tick);
            state
                .live
                .publish_event(contest_id, &event, std::slice::from_ref(&live_game));
        };
        publish(PASSING_CORRECTION_TICK - 1);
        let before = points(&state);
        publish(PASSING_CORRECTION_TICK);
        let after = points(&state);
        assert!(
            after < before,
            "the correction tick decreases the live score: {before} -> {after}"
        );
    }

    #[sqlx::test]
    async fn dev_feed_publishes_finished_games_at_start(pool: sqlx::SqlitePool) {
        use crate::contests::ContestId;
        use crate::entries::NflPlayerId;
        use crate::tests::TestApp;
        use nfl_data::{PlayerWeekStats, Season, TeamWeekStats, WeeklyRosterEntry};

        let now = time::macros::datetime!(2025-09-07 22:00 UTC);
        let kickoff = now - ::time::Duration::hours(5);
        let game = factories::game_at("2025_01_BUF_KC", 1, kickoff);
        let app = TestApp::from_pool_at_with_live_dev_feed(pool.clone(), now, true).await;

        let mut qb = factories::player("00-A", "Alpha Runner");
        qb.espn_id = Some("live-alpha".into());
        app.nfl
            .seed_for_test(&[qb], std::slice::from_ref(&game))
            .await
            .unwrap();
        app.nfl
            .seed_weekly_roster_for_test(&[WeeklyRosterEntry {
                espn_id: Some("live-alpha".into()),
                position: Some("QB".into()),
                ..factories::weekly_roster_entry("00-A", "Alpha Runner", 1)
            }])
            .await
            .unwrap();

        let qb_stats = PlayerWeekStats {
            gsis_id: "00-A".into(),
            team: TeamAbbr("KC".into()),
            opponent: Some(TeamAbbr("BUF".into())),
            passing_yards: 400,
            ..factories::player_stats("00-A", 1)
        };
        let kc_def = TeamWeekStats {
            gsis_game_id: "2025_01_BUF_KC".into(),
            ..factories::defense_stats("KC", "BUF", 1)
        };
        let buf_def = TeamWeekStats {
            gsis_game_id: "2025_01_BUF_KC".into(),
            ..factories::defense_stats("BUF", "KC", 1)
        };
        app.nfl
            .seed_week_stats_for_test(
                Season(2025),
                std::slice::from_ref(&qb_stats),
                &[kc_def, buf_def],
            )
            .await
            .unwrap();

        let contest = factories::contest_with_games(&pool, "Week 1", &["2025_01_BUF_KC"]).await;
        let contest_id = ContestId(contest);

        let mut config = (*app.state.config).clone();
        config.live_dev_feed_tick_ms = 1;
        let state = AppState {
            config: Arc::new(config),
            ..app.state.clone()
        };

        run(state.clone()).await;

        let snapshot = state
            .live
            .snapshot(contest_id)
            .expect("feed published a snapshot");
        let game_state = snapshot
            .game("2025_01_BUF_KC")
            .expect("game state in snapshot");
        assert_eq!(
            game_state.phase,
            LiveGamePhase::Final,
            "a game that ended before the pinned now settles immediately"
        );
        let qb = snapshot
            .stats
            .players
            .get(&NflPlayerId("00-A".into()))
            .expect("qb resolved into the snapshot");
        assert_eq!(
            *qb, qb_stats,
            "finished games publish the real finals line, not a ramp"
        );
    }
}
