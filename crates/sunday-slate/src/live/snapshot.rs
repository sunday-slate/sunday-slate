use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use nfl_data::{
    Game, LiveGame, LiveGamePhase, LiveGameSnapshot, LivePlayerStats, LiveTeamStats,
    PlayerWeekStats, ProviderOutcome, TeamAbbr, TeamWeekStats,
};
use time::OffsetDateTime;

use crate::contests::ContestId;
use crate::entries::{Lineup, NflPlayerId, RosterSlot};
use crate::live::identity::{IdentityMatch, LiveIdentityIndex};
use crate::live::{Coverage, LiveContestSnapshot, LiveError, LiveSlotStatus, LiveTotal};
use crate::player_identity::{canonical_team, normalize_team, provider_team};
use crate::scoring::{self, WeekStats};
use tank01_data::PollEvent;

#[derive(Debug, Clone, PartialEq)]
pub struct LiveGameState {
    pub game: Game,
    pub provider_game_id: Option<String>,
    pub phase: LiveGamePhase,
    pub period: Option<String>,
    pub clock: Option<String>,
    pub home_score: Option<i32>,
    pub away_score: Option<i32>,
    pub status_observed: bool,
    pub players: HashMap<NflPlayerId, PlayerWeekStats>,
    pub player_section_present: bool,
    pub player_identity_incomplete: bool,
    pub player_observed_at: Option<OffsetDateTime>,
    pub player_stale: bool,
    pub defenses: HashMap<TeamAbbr, TeamWeekStats>,
    pub defense_observed_at: HashMap<TeamAbbr, OffsetDateTime>,
    pub defense_stale: HashMap<TeamAbbr, bool>,
    pub last_error: Option<LiveError>,
}

impl LiveGameState {
    fn new(game: Game, provider_game_id: Option<String>, now: OffsetDateTime) -> Self {
        let phase = if game.kickoff_eastern().is_some_and(|kickoff| now < kickoff) {
            LiveGamePhase::Scheduled
        } else {
            LiveGamePhase::Unknown
        };
        Self {
            game,
            provider_game_id,
            phase,
            period: None,
            clock: None,
            home_score: None,
            away_score: None,
            status_observed: false,
            players: HashMap::new(),
            player_section_present: false,
            player_identity_incomplete: false,
            player_observed_at: None,
            player_stale: false,
            defenses: HashMap::new(),
            defense_observed_at: HashMap::new(),
            defense_stale: HashMap::new(),
            last_error: None,
        }
    }

    pub fn team_is_scheduled(&self, team: &TeamAbbr) -> bool {
        normalize_team(&self.game.home_team.0) == normalize_team(&team.0)
            || normalize_team(&self.game.away_team.0) == normalize_team(&team.0)
    }
    pub fn defense_is_stale(&self, team: &TeamAbbr) -> bool {
        self.defense_stale
            .iter()
            .find(|(candidate, _)| normalize_team(&candidate.0) == normalize_team(&team.0))
            .map(|(_, stale)| *stale)
            .unwrap_or(false)
    }

    pub(crate) fn player_is_stale_at(&self, now: OffsetDateTime) -> bool {
        self.phase_requires_observation()
            && (self.player_stale
                || self
                    .player_observed_at
                    .is_none_or(|at| now - at > time::Duration::seconds(60)))
    }

    pub(crate) fn defense_is_stale_at(&self, team: &TeamAbbr, now: OffsetDateTime) -> bool {
        self.phase_requires_observation()
            && (self.defense_is_stale(team)
                || self
                    .defense_observed_at
                    .iter()
                    .find(|(candidate, _)| normalize_team(&candidate.0) == normalize_team(&team.0))
                    .is_none_or(|(_, at)| now - *at > time::Duration::seconds(60)))
    }

    fn phase_requires_observation(&self) -> bool {
        self.phase == LiveGamePhase::InProgress
    }
}

pub struct SnapshotReducer {
    contest: ContestId,
    games: Vec<Game>,
    identity: Arc<LiveIdentityIndex>,
    game_states: HashMap<String, LiveGameState>,
    by_provider_id: HashMap<String, String>,
    revision: u64,
    latest_error: Option<LiveError>,
    identity_outcomes: BTreeMap<String, IdentityMatch>,
}

impl SnapshotReducer {
    pub fn new(
        contest: ContestId,
        games: Vec<Game>,
        identity: Arc<LiveIdentityIndex>,
        now: OffsetDateTime,
    ) -> Self {
        let mut game_states = HashMap::with_capacity(games.len());
        let mut by_provider_id = HashMap::with_capacity(games.len());
        for game in &games {
            let provider_id = provider_game_id(game);
            if let Some(provider_id) = &provider_id {
                by_provider_id.insert(provider_id.clone(), game.gsis_game_id.clone());
            }
            game_states.insert(
                game.gsis_game_id.clone(),
                LiveGameState::new(game.clone(), provider_id, now),
            );
        }
        Self {
            contest,
            games,
            identity,
            game_states,
            by_provider_id,
            revision: 0,
            latest_error: None,
            identity_outcomes: BTreeMap::new(),
        }
    }

    pub fn accepts_event(&self, event: &PollEvent) -> bool {
        match event {
            PollEvent::BoxScore { game, .. } => self
                .game_states
                .get(&game.gsis_game_id)
                .is_some_and(|state| {
                    state.provider_game_id.as_deref() == Some(game.provider_game_id.as_str())
                }),
            PollEvent::Scoreboard { .. } | PollEvent::RunFinished { .. } => true,
        }
    }

    pub fn apply(&mut self, event: &PollEvent, feed_games: &[LiveGame]) -> LiveContestSnapshot {
        match event {
            PollEvent::Scoreboard { response, .. } => self.apply_scoreboard(response, feed_games),
            PollEvent::BoxScore { game, response, .. } if self.accepts_event(event) => {
                self.apply_box_score(game, response)
            }
            PollEvent::BoxScore { .. } | PollEvent::RunFinished { .. } => {}
        }
        self.revision = self.revision.saturating_add(1);
        self.snapshot()
    }

    pub fn fail(&mut self, feed_games: &[LiveGame]) {
        let error = Some(LiveError::Provider);
        self.latest_error = error;
        for game in feed_games {
            let Some(state) = self.game_states.get_mut(&game.gsis_game_id) else {
                continue;
            };
            if state.provider_game_id.as_deref() != Some(game.provider_game_id.as_str()) {
                continue;
            }
            state.last_error = error;
            state.player_stale = true;
            for team in [&state.game.home_team, &state.game.away_team] {
                state.defense_stale.insert(team.clone(), true);
            }
        }
    }

    pub fn snapshot(&self) -> LiveContestSnapshot {
        let mut stats = WeekStats::default();
        let mut games = self.game_states.values().collect::<Vec<_>>();
        games.sort_by(|a, b| a.game.gsis_game_id.cmp(&b.game.gsis_game_id));
        for state in games {
            stats.players.extend(state.players.clone());
            stats.defenses.extend(state.defenses.clone());
        }
        LiveContestSnapshot {
            contest: self.contest,
            games: self.games.clone(),
            stats,
            game_states: self.game_states.clone(),
            identity: self.identity.clone(),
            identity_outcomes: self.identity_outcomes.clone(),
            latest_error: self.latest_error,
            revision: self.revision,
        }
    }

    fn apply_scoreboard(
        &mut self,
        response: &nfl_data::ProviderResponse<nfl_data::LiveScoreboard>,
        feed_games: &[LiveGame],
    ) {
        match &response.outcome {
            ProviderOutcome::Value(scoreboard) => {
                self.latest_error = None;
                for update in &scoreboard.games {
                    if !feed_games
                        .iter()
                        .any(|game| game.provider_game_id == update.provider_game_id)
                    {
                        continue;
                    }
                    let Some(gsis_id) = self.by_provider_id.get(&update.provider_game_id).cloned()
                    else {
                        continue;
                    };
                    let Some(state) = self.game_states.get_mut(&gsis_id) else {
                        continue;
                    };
                    state.last_error = None;
                    state.phase = update.phase;
                    state.period = update.period.clone();
                    state.clock = update.clock.clone();
                    state.home_score = update.home_score;
                    state.away_score = update.away_score;
                    state.status_observed = true;
                }
            }
            ProviderOutcome::Pregame { .. } | ProviderOutcome::Error(_) => {
                let error = Some(LiveError::Provider);
                self.latest_error = error;
                for game in feed_games {
                    let Some(gsis_id) = self.by_provider_id.get(&game.provider_game_id).cloned()
                    else {
                        continue;
                    };
                    let Some(state) = self.game_states.get_mut(&gsis_id) else {
                        continue;
                    };
                    state.last_error = error;
                    state.player_stale = true;
                    for team in [&state.game.home_team, &state.game.away_team] {
                        state.defense_stale.insert(team.clone(), true);
                    }
                }
            }
        }
    }

    fn apply_box_score(
        &mut self,
        game: &LiveGame,
        response: &nfl_data::ProviderResponse<LiveGameSnapshot>,
    ) {
        let state_id = game.gsis_game_id.clone();

        match &response.outcome {
            ProviderOutcome::Value(snapshot) => {
                self.latest_error = None;
                let adapted_players = snapshot.players.as_ref().map(|players| {
                    let mut adapted = HashMap::new();
                    let mut identity_incomplete = false;
                    for player in players {
                        let identity = self.resolve_player(player);
                        let Some(gsis_id) = (match identity {
                            IdentityMatch::Matched(gsis_id) => Some(gsis_id),
                            other => {
                                identity_incomplete = true;
                                self.identity_outcomes
                                    .insert(provider_identity_key(player), other);
                                None
                            }
                        }) else {
                            continue;
                        };
                        adapted.insert(
                            gsis_id.clone(),
                            adapt_player(
                                snapshot,
                                player,
                                gsis_id.clone(),
                                self.identity.scheduled_team(&gsis_id),
                            ),
                        );
                    }
                    (adapted, identity_incomplete)
                });
                let adapted_defenses = snapshot.defenses.as_ref().map(|defenses| {
                    defenses
                        .iter()
                        .filter(|defense| defense.dst_present && defense.team_stats_present)
                        .map(|defense| {
                            let team = canonical_team(&defense.team);
                            (team.clone(), adapt_defense(snapshot, defense, team))
                        })
                        .collect::<HashMap<_, _>>()
                });

                let state = self
                    .game_states
                    .get_mut(&state_id)
                    .expect("game state was checked above");
                state.last_error = None;
                state.phase = snapshot.phase;
                state.period = snapshot.period.clone();
                state.clock = snapshot.clock.clone();
                state.home_score = snapshot.home_score;
                state.away_score = snapshot.away_score;
                state.status_observed = true;
                if let Some((players, identity_incomplete)) = adapted_players {
                    state.players = players;
                    state.player_section_present = true;
                    state.player_identity_incomplete = identity_incomplete;
                    state.player_observed_at = Some(snapshot.observed_at);
                    state.player_stale = false;
                }
                if let Some(defenses) = adapted_defenses {
                    for (team, defense) in defenses {
                        state.defenses.insert(team.clone(), defense);
                        state
                            .defense_observed_at
                            .insert(team.clone(), snapshot.observed_at);
                        state.defense_stale.insert(team, false);
                    }
                }
            }
            ProviderOutcome::Pregame { .. } | ProviderOutcome::Error(_) => {
                let error = Some(LiveError::Provider);
                self.latest_error = error;
                let state = self
                    .game_states
                    .get_mut(&state_id)
                    .expect("game state was checked above");
                state.last_error = error;
                state.player_stale = true;
                for team in [&state.game.home_team, &state.game.away_team] {
                    state.defense_stale.insert(team.clone(), true);
                }
            }
        }
    }

    fn resolve_player(&self, player: &LivePlayerStats) -> IdentityMatch {
        self.identity.resolve(
            player.espn_id.as_ref(),
            player.name.as_deref(),
            player.team.as_ref(),
            player.position.as_deref(),
        )
    }
}

fn opposing_team<'a>(game: &'a LiveGame, team: Option<&TeamAbbr>) -> &'a TeamAbbr {
    match team {
        Some(team) if *team == game.home_team => &game.away_team,
        _ => &game.home_team,
    }
}

fn adapt_player(
    snapshot: &LiveGameSnapshot,
    row: &LivePlayerStats,
    gsis_id: NflPlayerId,
    known_team: Option<&TeamAbbr>,
) -> PlayerWeekStats {
    let team = known_team
        .or(row.team.as_ref())
        .unwrap_or(&snapshot.game.home_team);
    let opponent = row
        .opponent
        .as_ref()
        .unwrap_or_else(|| opposing_team(&snapshot.game, Some(team)));
    PlayerWeekStats {
        season: snapshot.game.season,
        week: snapshot.game.week,
        season_type: snapshot.game.season_type,
        gsis_id: gsis_id.0,
        team: canonical_team(team),
        opponent: Some(canonical_team(opponent)),
        completions: row.completions,
        attempts: row.attempts,
        passing_yards: row.passing_yards,
        passing_tds: row.passing_tds,
        passing_interceptions: row.passing_interceptions,
        rushing_attempts: row.rushing_attempts,
        rushing_yards: row.rushing_yards,
        rushing_tds: row.rushing_tds,
        targets: row.targets,
        receptions: row.receptions,
        receiving_yards: row.receiving_yards,
        receiving_tds: row.receiving_tds,
        fumbles_lost: row.fumbles_lost,
        two_point_conversions: row.two_point_conversions,
        special_teams_tds: row.special_teams_tds,
        fumble_recovery_tds: row.fumble_recovery_tds,
    }
}

fn adapt_defense(
    snapshot: &LiveGameSnapshot,
    row: &LiveTeamStats,
    team: TeamAbbr,
) -> TeamWeekStats {
    TeamWeekStats {
        season: snapshot.game.season,
        week: snapshot.game.week,
        season_type: snapshot.game.season_type,
        team,
        opponent: canonical_team(&row.opponent),
        gsis_game_id: snapshot.game.gsis_game_id.clone(),
        sacks: row.sacks,
        interceptions: row.interceptions,
        fumble_recoveries: row.fumble_recoveries,
        safeties: row.safeties,
        touchdowns: row.touchdowns,
        blocked_kicks: row.blocked_kicks,
        conversion_returns: row.conversion_returns,
        points_allowed: row.points_allowed,
    }
}

pub fn provider_game_id(game: &Game) -> Option<String> {
    let date = game.kickoff_eastern()?.date();
    let date = format!(
        "{:04}{:02}{:02}",
        date.year(),
        date.month() as u8,
        date.day()
    );
    Some(format!(
        "{}_{}@{}",
        date,
        provider_team(&game.away_team.0),
        provider_team(&game.home_team.0)
    ))
}

/// The game in the provider id space it polls; `None` without a kickoff date.
pub fn live_game(game: &Game) -> Option<LiveGame> {
    Some(LiveGame {
        gsis_game_id: game.gsis_game_id.clone(),
        provider_game_id: provider_game_id(game)?,
        season: game.season,
        week: game.week,
        season_type: game.season_type,
        kickoff: game.kickoff,
        home_team: game.home_team.clone(),
        away_team: game.away_team.clone(),
    })
}

fn provider_identity_key(player: &LivePlayerStats) -> String {
    player
        .espn_id
        .as_ref()
        .map(|id| format!("espn:{}", id.0))
        .or_else(|| player.name.as_ref().map(|name| format!("name:{name}")))
        .unwrap_or_else(|| "unknown".to_owned())
}

#[derive(Debug, Clone, Copy)]
enum LiveContribution {
    ScheduledZero,
    Observed(f64),
    Unavailable,
}

impl LiveContribution {
    fn points(self) -> Option<f64> {
        match self {
            Self::ScheduledZero => Some(0.0),
            Self::Observed(points) => Some(points),
            Self::Unavailable => None,
        }
    }
}

pub fn score_live_slot(
    lineup: &Lineup,
    slot: RosterSlot,
    snapshot: &LiveContestSnapshot,
    now: OffsetDateTime,
) -> LiveSlotStatus {
    let (contribution, state, stale) = match slot {
        RosterSlot::Def => {
            let state = snapshot.game_for_team(&lineup.def);
            (
                score_live_defense(&lineup.def, state, now),
                state,
                state.is_some_and(|state| state.defense_is_stale_at(&lineup.def, now)),
            )
        }
        slot => {
            let id = lineup
                .player_slots()
                .into_iter()
                .find_map(|(candidate, id)| (candidate == slot).then_some(id));
            let state = id.and_then(|id| {
                snapshot
                    .identity
                    .scheduled_team(id)
                    .and_then(|team| snapshot.game_for_team(team))
            });
            (
                id.map_or(LiveContribution::Unavailable, |id| {
                    score_live_player(id, state, snapshot, now)
                }),
                state,
                state.is_some_and(|state| state.player_is_stale_at(now)),
            )
        }
    };
    LiveSlotStatus {
        total: LiveTotal {
            points: contribution.points(),
            coverage: contribution
                .points()
                .map_or(Coverage::Unavailable, |_| Coverage::Complete),
        },
        phase: state.map(|state| state.phase),
        period: state.and_then(|state| state.period.clone()),
        clock: state.and_then(|state| state.clock.clone()),
        stale,
    }
}

pub fn score_live_lineup(
    lineup: &Lineup,
    snapshot: &LiveContestSnapshot,
    now: OffsetDateTime,
) -> LiveTotal {
    let mut values = Vec::with_capacity(9);
    for player_id in lineup.player_ids() {
        let state = snapshot
            .identity
            .scheduled_team(player_id)
            .and_then(|team| snapshot.game_for_team(team));
        values.push(score_live_player(player_id, state, snapshot, now));
    }
    let defense_state = snapshot.game_for_team(&lineup.def);
    values.push(score_live_defense(&lineup.def, defense_state, now));

    let unavailable = values
        .iter()
        .any(|value| matches!(value, LiveContribution::Unavailable));
    let observed = values
        .iter()
        .filter_map(|value| match value {
            LiveContribution::Observed(points) => Some(*points),
            LiveContribution::ScheduledZero | LiveContribution::Unavailable => None,
        })
        .count();
    let available = values.iter().filter_map(|value| value.points()).sum();
    if unavailable && observed == 0 {
        LiveTotal {
            points: None,
            coverage: Coverage::Unavailable,
        }
    } else if unavailable {
        LiveTotal {
            points: Some(available),
            coverage: Coverage::Partial,
        }
    } else {
        LiveTotal {
            points: Some(available),
            coverage: Coverage::Complete,
        }
    }
}

/// Full mark for one entry's meter: DEF counts and duplicate-game picks each
/// count their game's minutes (9 slots × 60 regulation minutes).
pub const LINEUP_REGULATION_MINUTES: f64 = 9.0 * 60.0;

pub fn lineup_minutes_remaining(
    lineup: &Lineup,
    snapshot: &LiveContestSnapshot,
    now: OffsetDateTime,
) -> f64 {
    let mut total = 0.0;
    for player_id in lineup.player_ids() {
        let state = snapshot
            .identity
            .scheduled_team(player_id)
            .and_then(|team| snapshot.game_for_team(team));
        total += game_minutes_remaining(state, now);
    }
    total += game_minutes_remaining(snapshot.game_for_team(&lineup.def), now);
    total.clamp(0.0, LINEUP_REGULATION_MINUTES)
}

fn game_minutes_remaining(state: Option<&LiveGameState>, now: OffsetDateTime) -> f64 {
    let Some(state) = state else {
        return 60.0;
    };
    match state.phase {
        // An unknowable phase is a data gap, not a late game: showing no
        // minutes would false-red every entry.
        LiveGamePhase::Scheduled | LiveGamePhase::Unknown => 60.0,
        LiveGamePhase::Final => 0.0,
        LiveGamePhase::InProgress => {
            let period = period_number(state.period.as_deref());
            let clock = clock_seconds(state.clock.as_deref());
            match (period, clock) {
                (Some(n), _) if n >= 5 => 0.0,
                (Some(n), Some(secs)) if (1..=4).contains(&n) => {
                    60.0 - 15.0 * (n - 1) as f64 - (900.0 - secs as f64) / 60.0
                }
                (Some(n), None) if (1..=4).contains(&n) => {
                    // A mid-quarter provider glitch; the next frame corrects it.
                    60.0 - 15.0 * (n - 1) as f64
                }
                _ => wall_clock_minutes_remaining(state, now),
            }
        }
    }
}

fn period_number(period: Option<&str>) -> Option<u32> {
    let digits: String = period?.chars().filter(char::is_ascii_digit).collect();
    digits.parse().ok()
}

fn clock_seconds(clock: Option<&str>) -> Option<u32> {
    let (minutes, seconds) = clock?.split_once(':')?;
    let minutes: u64 = minutes.trim().parse().ok()?;
    let seconds: u64 = seconds.trim().parse().ok()?;
    Some((minutes * 60 + seconds).min(900) as u32)
}

fn wall_clock_minutes_remaining(state: &LiveGameState, now: OffsetDateTime) -> f64 {
    let Some(kickoff) = state.game.kickoff_eastern() else {
        return 60.0;
    };
    let elapsed = (now - kickoff).whole_seconds() as f64;
    let drained = (elapsed / (3.0 * 3600.0)).clamp(0.0, 1.0);
    60.0 * (1.0 - drained)
}

fn score_live_player(
    id: &NflPlayerId,
    state: Option<&LiveGameState>,
    snapshot: &LiveContestSnapshot,
    now: OffsetDateTime,
) -> LiveContribution {
    let Some(state) = state else {
        return LiveContribution::Unavailable;
    };
    match state.phase {
        LiveGamePhase::Scheduled => scheduled_contribution(state, now),
        LiveGamePhase::InProgress | LiveGamePhase::Final | LiveGamePhase::Unknown => {
            if let Some(stats) = state.players.get(id) {
                LiveContribution::Observed(scoring::score_player(stats).total)
            } else if state.player_section_present
                && !state.player_identity_incomplete
                && snapshot.identity.contains_gsis(id)
            {
                LiveContribution::Observed(0.0)
            } else {
                LiveContribution::Unavailable
            }
        }
    }
}

fn score_live_defense(
    team: &TeamAbbr,
    state: Option<&LiveGameState>,
    now: OffsetDateTime,
) -> LiveContribution {
    let Some(state) = state else {
        return LiveContribution::Unavailable;
    };
    match state.phase {
        LiveGamePhase::Scheduled => scheduled_contribution(state, now),
        LiveGamePhase::InProgress | LiveGamePhase::Final | LiveGamePhase::Unknown => state
            .defenses
            .iter()
            .find(|(candidate, _)| normalize_team(&candidate.0) == normalize_team(&team.0))
            .map(|(_, stats)| LiveContribution::Observed(scoring::score_defense(stats).total))
            .unwrap_or(LiveContribution::Unavailable),
    }
}

fn scheduled_contribution(state: &LiveGameState, now: OffsetDateTime) -> LiveContribution {
    if state.status_observed
        || state
            .game
            .kickoff_eastern()
            .is_some_and(|kickoff| now < kickoff)
    {
        LiveContribution::ScheduledZero
    } else {
        LiveContribution::Unavailable
    }
}

impl LiveContestSnapshot {
    pub fn game(&self, gsis_game_id: &str) -> Option<&LiveGameState> {
        self.game_states.get(gsis_game_id)
    }

    pub fn scored_games(&self) -> Vec<Game> {
        self.games
            .iter()
            .map(|game| {
                let mut game = game.clone();
                if let Some(state) = self.game(&game.gsis_game_id) {
                    game.home_score = state.home_score;
                    game.away_score = state.away_score;
                }
                game
            })
            .collect()
    }

    pub fn game_for_team(&self, team: &TeamAbbr) -> Option<&LiveGameState> {
        self.game_states
            .values()
            .find(|state| state.team_is_scheduled(team))
    }

    pub fn has_active_game(&self) -> bool {
        self.game_states
            .values()
            .any(|state| state.phase == LiveGamePhase::InProgress)
    }

    pub fn is_delayed(&self, now: OffsetDateTime) -> bool {
        self.game_states.values().any(|state| {
            state.phase_requires_observation()
                && (state.player_is_stale_at(now)
                    || state.defense_is_stale_at(&state.game.home_team, now)
                    || state.defense_is_stale_at(&state.game.away_team, now))
        })
    }
}
#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use nfl_data::{
        EspnPlayerId, Game, LiveGame, LiveGameSnapshot, LivePlayerStats, LiveProviderError,
        LiveTeamStats, Player, ProviderOutcome, ProviderResponse, TeamAbbr,
    };
    use time::macros::datetime;

    use super::*;
    use crate::entries::Lineup;
    use crate::tests::factories;

    fn game() -> Game {
        let mut game = factories::game("live", 1);
        game.kickoff = Some(datetime!(2025 - 09 - 07 17:00 UTC));
        game
    }

    fn identity() -> Arc<LiveIdentityIndex> {
        Arc::new(LiveIdentityIndex::from_sources(
            &[Player {
                espn_id: Some("e1".into()),
                position: Some("RB".into()),
                ..factories::player("p1", "A Runner")
            }],
            &[],
        ))
    }

    fn player_stats() -> LivePlayerStats {
        LivePlayerStats {
            espn_id: Some(EspnPlayerId("e1".into())),
            name: Some("A Runner".into()),
            team: Some(TeamAbbr("KC".into())),
            position: Some("RB".into()),
            opponent: Some(TeamAbbr("BUF".into())),
            completions: 0,
            attempts: 0,
            passing_tds: 0,
            passing_interceptions: 0,
            rushing_attempts: 1,
            rushing_tds: 0,
            targets: 0,
            receptions: 0,
            receiving_tds: 0,
            fumbles_lost: 0,
            two_point_conversions: 0,
            special_teams_tds: 0,
            fumble_recovery_tds: 0,
            passing_yards: 0,
            rushing_yards: 10,
            receiving_yards: 0,
        }
    }

    fn defense_stats() -> LiveTeamStats {
        LiveTeamStats {
            team: TeamAbbr("KC".into()),
            opponent: TeamAbbr("BUF".into()),
            sacks: 1,
            interceptions: 0,
            fumble_recoveries: 0,
            safeties: 0,
            touchdowns: 0,
            blocked_kicks: 0,
            conversion_returns: 0,
            points_allowed: 7,
            dst_present: true,
            team_stats_present: true,
        }
    }

    fn response(
        outcome: ProviderOutcome<LiveGameSnapshot>,
        observed_at: time::OffsetDateTime,
    ) -> ProviderResponse<LiveGameSnapshot> {
        ProviderResponse {
            requested_at: observed_at,
            received_at: observed_at,
            elapsed_ms: 1,
            http_status: Some(200),
            headers: BTreeMap::new(),
            raw_body: None,
            outcome,
        }
    }

    fn box_event(
        game: &LiveGame,
        snapshot: LiveGameSnapshot,
        observed_at: time::OffsetDateTime,
    ) -> PollEvent {
        PollEvent::BoxScore {
            sequence: 1,
            game: game.clone(),
            request: tank01_data::PollRequest {
                endpoint: "box".into(),
                query: BTreeMap::new(),
            },
            play_by_play: false,
            changed: true,
            response: response(ProviderOutcome::Value(snapshot), observed_at),
        }
    }

    fn lineup() -> Lineup {
        let id = crate::entries::NflPlayerId("p1".into());
        Lineup {
            qb: id.clone(),
            rb1: id.clone(),
            rb2: id.clone(),
            wr1: id.clone(),
            wr2: id.clone(),
            wr3: id.clone(),
            te: id.clone(),
            flex: id,
            def: TeamAbbr("KC".into()),
        }
    }

    #[test]
    fn omitted_sections_preserve_last_good_data_and_errors_mark_it_stale() {
        let game = game();
        let live_game = live_game(&game).expect("fixture kickoff");
        let observed_at = datetime!(2025 - 09 - 07 18:00 UTC);
        let mut reducer = SnapshotReducer::new(ContestId(1), vec![game], identity(), observed_at);
        let first = LiveGameSnapshot {
            game: live_game.clone(),
            observed_at,
            phase: LiveGamePhase::InProgress,
            period: Some("1".into()),
            clock: Some("10:00".into()),
            home_score: Some(7),
            away_score: Some(0),
            players: Some(vec![player_stats()]),
            defenses: Some(vec![defense_stats()]),
        };

        let snapshot = reducer.apply(
            &box_event(&live_game, first, observed_at),
            std::slice::from_ref(&live_game),
        );
        let state = snapshot.game("live").expect("live game");
        assert_eq!(state.players.len(), 1);
        assert_eq!(state.defenses.len(), 1);

        let second_at = datetime!(2025 - 09 - 07 18:00:20 UTC);
        let second = LiveGameSnapshot {
            game: live_game.clone(),
            observed_at: second_at,
            phase: LiveGamePhase::InProgress,
            period: Some("1".into()),
            clock: Some("09:40".into()),
            home_score: Some(7),
            away_score: Some(3),
            players: None,
            defenses: None,
        };
        let snapshot = reducer.apply(
            &box_event(&live_game, second, second_at),
            std::slice::from_ref(&live_game),
        );
        let state = snapshot.game("live").expect("live game");
        assert_eq!(state.players.len(), 1);
        assert_eq!(state.defenses.len(), 1);
        assert_eq!(state.away_score, Some(3));
        let before_failure =
            score_live_lineup(&lineup(), &snapshot, second_at + time::Duration::seconds(1));

        let error_at = datetime!(2025 - 09 - 07 18:01 UTC);
        let error = response(
            ProviderOutcome::Error(LiveProviderError::Transport {
                message: "timeout".into(),
            }),
            error_at,
        );
        let snapshot = reducer.apply(
            &PollEvent::BoxScore {
                sequence: 2,
                game: live_game.clone(),
                request: tank01_data::PollRequest {
                    endpoint: "box".into(),
                    query: BTreeMap::new(),
                },
                play_by_play: false,
                changed: false,
                response: error,
            },
            std::slice::from_ref(&live_game),
        );
        let after_failure = score_live_lineup(&lineup(), &snapshot, error_at);
        assert_eq!(after_failure.points, before_failure.points);
        assert_eq!(after_failure.coverage, before_failure.coverage);
        let state = snapshot.game("live").expect("live game");
        assert_eq!(state.players.len(), 1);
        assert_eq!(state.defenses.len(), 1);
        assert!(state.player_stale);
        assert!(state.last_error.is_some());
    }

    #[test]
    fn scheduled_slots_are_zero_but_missing_live_defense_is_unavailable() {
        let game = game();
        let live_game = live_game(&game).expect("fixture kickoff");
        let identity = identity();
        let scheduled_at = datetime!(2025 - 09 - 07 16:00 UTC);
        let mut reducer = SnapshotReducer::new(ContestId(1), vec![game], identity, scheduled_at);
        let scheduled = reducer.snapshot();
        let scheduled_total = score_live_lineup(&lineup(), &scheduled, scheduled_at);
        assert_eq!(scheduled_total.points, Some(0.0));
        assert_eq!(scheduled_total.coverage, Coverage::Complete);

        let observed_at = datetime!(2025 - 09 - 07 18:00 UTC);
        let live = LiveGameSnapshot {
            game: live_game.clone(),
            observed_at,
            phase: LiveGamePhase::InProgress,
            period: Some("1".into()),
            clock: Some("10:00".into()),
            home_score: Some(7),
            away_score: Some(0),
            players: Some(vec![player_stats()]),
            defenses: Some(Vec::new()),
        };
        let snapshot = reducer.apply(
            &box_event(&live_game, live, observed_at),
            std::slice::from_ref(&live_game),
        );
        let defense = score_live_slot(&lineup(), RosterSlot::Def, &snapshot, observed_at);
        assert_eq!(defense.total.coverage, Coverage::Unavailable);
        assert_eq!(
            score_live_lineup(&lineup(), &snapshot, observed_at).coverage,
            Coverage::Partial
        );
    }
    #[test]
    fn final_games_are_not_delayed_by_stale_state() {
        let game = game();
        let reducer = SnapshotReducer::new(
            ContestId(1),
            vec![game],
            identity(),
            datetime!(2025 - 09 - 07 18:00 UTC),
        );
        let mut snapshot = reducer.snapshot();
        let state = snapshot.game_states.get_mut("live").expect("live game");
        state.phase = LiveGamePhase::Final;
        state.last_error = Some(LiveError::Provider);
        state.player_stale = true;
        state.player_observed_at = Some(datetime!(2025 - 09 - 07 18:00 UTC));
        state.defense_observed_at.insert(
            state.game.home_team.clone(),
            datetime!(2025 - 09 - 07 18:00 UTC),
        );
        state
            .defense_stale
            .insert(state.game.home_team.clone(), true);
        state
            .defense_stale
            .insert(state.game.away_team.clone(), true);

        assert!(!snapshot.is_delayed(datetime!(2025 - 09 - 07 18:05 UTC)));
    }

    #[test]
    fn missing_live_defense_observation_is_delayed() {
        let game = game();
        let live_game = live_game(&game).expect("fixture kickoff");
        let observed_at = datetime!(2025 - 09 - 07 18:00 UTC);
        let mut reducer = SnapshotReducer::new(ContestId(1), vec![game], identity(), observed_at);
        let live = LiveGameSnapshot {
            game: live_game.clone(),
            observed_at,
            phase: LiveGamePhase::InProgress,
            period: Some("1".into()),
            clock: Some("10:00".into()),
            home_score: Some(7),
            away_score: Some(0),
            players: Some(vec![player_stats()]),
            defenses: Some(vec![defense_stats()]),
        };
        let snapshot = reducer.apply(
            &box_event(&live_game, live, observed_at),
            std::slice::from_ref(&live_game),
        );

        assert!(snapshot.is_delayed(observed_at + time::Duration::seconds(1)));
    }
    #[test]
    fn due_game_without_status_observation_is_unavailable() {
        let game = game();
        let now = datetime!(2025 - 09 - 07 18:00 UTC);
        let reducer = SnapshotReducer::new(ContestId(1), vec![game], identity(), now);
        let total = score_live_lineup(&lineup(), &reducer.snapshot(), now);

        assert_eq!(total.points, None);
        assert_eq!(total.coverage, Coverage::Unavailable);
    }

    fn minutes_state(
        phase: LiveGamePhase,
        period: Option<&str>,
        clock: Option<&str>,
    ) -> LiveGameState {
        LiveGameState {
            game: game(),
            provider_game_id: Some("live".into()),
            phase,
            period: period.map(str::to_string),
            clock: clock.map(str::to_string),
            home_score: Some(14),
            away_score: Some(7),
            status_observed: true,
            players: HashMap::new(),
            player_section_present: false,
            player_identity_incomplete: false,
            player_observed_at: None,
            player_stale: false,
            defenses: HashMap::new(),
            defense_observed_at: HashMap::new(),
            defense_stale: HashMap::new(),
            last_error: None,
        }
    }

    #[test]
    fn game_minutes_remaining_follows_phase_rules() {
        let now = datetime!(2025 - 09 - 07 18:00 UTC);
        let minutes = |state: LiveGameState| game_minutes_remaining(Some(&state), now);

        assert_eq!(game_minutes_remaining(None, now), 60.0);
        assert_eq!(
            minutes(minutes_state(LiveGamePhase::Scheduled, None, None)),
            60.0
        );
        assert_eq!(
            minutes(minutes_state(
                LiveGamePhase::Unknown,
                Some("3"),
                Some("05:00")
            )),
            60.0
        );
        assert_eq!(
            minutes(minutes_state(
                LiveGamePhase::Final,
                Some("4"),
                Some("00:00")
            )),
            0.0
        );
        assert_eq!(
            minutes(minutes_state(
                LiveGamePhase::InProgress,
                Some("2"),
                Some("08:00")
            )),
            38.0
        );
        assert_eq!(
            minutes(minutes_state(
                LiveGamePhase::InProgress,
                Some("Q2"),
                Some("08:00")
            )),
            38.0,
            "provider decorations around the quarter digit parse"
        );
        assert_eq!(
            minutes(minutes_state(
                LiveGamePhase::InProgress,
                Some("2"),
                Some("00:00")
            )),
            30.0
        );
        assert_eq!(
            minutes(minutes_state(
                LiveGamePhase::InProgress,
                Some("1"),
                Some("15:00")
            )),
            60.0
        );
        assert_eq!(
            minutes(minutes_state(
                LiveGamePhase::InProgress,
                Some("5"),
                Some("10:00")
            )),
            0.0
        );
        assert_eq!(
            minutes(minutes_state(LiveGamePhase::InProgress, Some("3"), None)),
            30.0
        );
        assert_eq!(
            minutes(minutes_state(
                LiveGamePhase::InProgress,
                Some("1"),
                Some("99:00")
            )),
            60.0,
            "clock seconds cap at a full quarter"
        );

        let kickoff = minutes_state(LiveGamePhase::InProgress, None, None);
        assert_eq!(
            game_minutes_remaining(Some(&kickoff), datetime!(2025 - 09 - 07 18:30 UTC)),
            30.0,
            "wall-clock fallback halfway through the ~3h window"
        );
        assert_eq!(
            game_minutes_remaining(Some(&kickoff), datetime!(2025 - 09 - 07 16:50 UTC)),
            60.0,
            "a hit-before-kickoff InProgress state clamps, never overdrains"
        );
    }

    fn minutes_snapshot(
        phase: LiveGamePhase,
        period: Option<&str>,
        clock: Option<&str>,
    ) -> LiveContestSnapshot {
        let state = minutes_state(phase, period, clock);
        LiveContestSnapshot {
            contest: ContestId(1),
            games: vec![state.game.clone()],
            stats: WeekStats::default(),
            game_states: HashMap::from([("live".into(), state)]),
            identity: Arc::new(LiveIdentityIndex::from_sources(
                &[factories::player("p1", "A Runner")],
                &[],
            )),
            identity_outcomes: Default::default(),
            latest_error: None,
            revision: 1,
        }
    }

    #[test]
    fn lineup_minutes_sums_all_nine_slots_and_clamps() {
        let now = datetime!(2025 - 09 - 07 18:00 UTC);
        let final_snapshot = minutes_snapshot(LiveGamePhase::Final, Some("4"), Some("00:00"));
        assert_eq!(
            lineup_minutes_remaining(&lineup(), &final_snapshot, now),
            0.0
        );

        let live_snapshot = minutes_snapshot(LiveGamePhase::InProgress, Some("2"), Some("08:00"));
        assert_eq!(
            lineup_minutes_remaining(&lineup(), &live_snapshot, now),
            342.0,
            "nine slots x 38 minutes each"
        );

        let mut identityless = live_snapshot;
        identityless.identity = Arc::new(LiveIdentityIndex::empty());
        assert_eq!(
            lineup_minutes_remaining(&lineup(), &identityless, now),
            518.0,
            "player gaps hold at a full slot; the def still counts its game's 38"
        );

        let mut unresolvable = minutes_snapshot(LiveGamePhase::Scheduled, None, None);
        unresolvable.game_states.clear();
        unresolvable.identity = Arc::new(LiveIdentityIndex::empty());
        assert_eq!(
            lineup_minutes_remaining(&lineup(), &unresolvable, now),
            540.0,
            "nine unresolvable slots fill the bar to the full mark"
        );
    }
}
