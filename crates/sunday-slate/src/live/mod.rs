mod coordinator;
pub(crate) mod dev_feed;
pub mod identity;
mod snapshot;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};

use nfl_data::{Game, LiveGamePhase, Season, SeasonType, TeamAbbr, Week};
use tokio::sync::watch;

use crate::contests::ContestId;
use crate::contests::service::slate_locked;
use crate::live::identity::LiveIdentityIndex;
use crate::live::snapshot::SnapshotReducer;
use crate::scoring::WeekStats;

pub(crate) use coordinator::run_coordinator;
pub use snapshot::{
    LINEUP_REGULATION_MINUTES, LiveGameState, lineup_minutes_remaining, live_game,
    provider_game_id, score_live_lineup, score_live_slot,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Coverage {
    Complete,
    Partial,
    #[default]
    Unavailable,
}

impl Coverage {
    pub fn is_complete(self) -> bool {
        self == Self::Complete
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LiveTotal {
    pub points: Option<f64>,
    pub coverage: Coverage,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LiveSlotStatus {
    pub total: LiveTotal,
    pub phase: Option<LiveGamePhase>,
    pub period: Option<String>,
    pub clock: Option<String>,
    pub stale: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveError {
    Provider,
    Identity,
}

#[derive(Debug, Clone)]
pub struct LiveContestSnapshot {
    pub contest: ContestId,
    pub games: Vec<Game>,
    pub stats: WeekStats,
    pub game_states: std::collections::HashMap<String, LiveGameState>,
    pub identity: Arc<LiveIdentityIndex>,
    pub identity_outcomes: BTreeMap<String, identity::IdentityMatch>,
    pub latest_error: Option<LiveError>,
    pub revision: u64,
}

impl LiveContestSnapshot {
    pub fn phase(&self) -> Option<LiveGamePhase> {
        self.game_states
            .values()
            .map(|state| state.phase)
            .max_by_key(|phase| match phase {
                LiveGamePhase::Scheduled => 0,
                LiveGamePhase::Unknown => 1,
                LiveGamePhase::InProgress => 2,
                LiveGamePhase::Final => 3,
            })
    }

    pub fn score(&self, team: &TeamAbbr) -> Option<i32> {
        self.game_states.values().find_map(|state| {
            if state.game.home_team == *team {
                state.home_score
            } else if state.game.away_team == *team {
                state.away_score
            } else {
                None
            }
        })
    }
}

struct ContestRegistration {
    games: Vec<Game>,
    identity: Arc<LiveIdentityIndex>,
    locked_at: Option<time::OffsetDateTime>,
}

struct HubState {
    snapshots: std::collections::HashMap<ContestId, Arc<LiveContestSnapshot>>,
    reducers: std::collections::HashMap<ContestId, SnapshotReducer>,
    registrations: std::collections::HashMap<ContestId, ContestRegistration>,
    senders: std::collections::HashMap<ContestId, watch::Sender<u64>>,
    revisions: std::collections::HashMap<ContestId, u64>,
}

struct HubInner {
    enabled: bool,
    state: Mutex<HubState>,
    shutdown: watch::Sender<bool>,
}

#[derive(Clone)]
pub struct LiveContestHub {
    inner: Arc<HubInner>,
}

impl LiveContestHub {
    pub fn new(enabled: bool) -> Self {
        let (shutdown, _) = watch::channel(false);
        Self {
            inner: Arc::new(HubInner {
                enabled,
                state: Mutex::new(HubState {
                    snapshots: std::collections::HashMap::new(),
                    reducers: std::collections::HashMap::new(),
                    registrations: std::collections::HashMap::new(),
                    senders: std::collections::HashMap::new(),
                    revisions: std::collections::HashMap::new(),
                }),
                shutdown,
            }),
        }
    }

    pub fn enabled(&self) -> bool {
        self.inner.enabled
    }

    pub fn snapshot(&self, id: ContestId) -> Option<Arc<LiveContestSnapshot>> {
        self.inner
            .state
            .lock()
            .expect("live hub mutex poisoned")
            .snapshots
            .get(&id)
            .cloned()
    }

    pub fn subscribe(&self, id: ContestId) -> watch::Receiver<u64> {
        let mut state = self.inner.state.lock().expect("live hub mutex poisoned");
        let revision = *state.revisions.entry(id).or_insert(0);
        state
            .senders
            .entry(id)
            .or_insert_with(|| watch::channel(revision).0)
            .subscribe()
    }

    pub(crate) fn shutdown_receiver(&self) -> watch::Receiver<bool> {
        self.inner.shutdown.subscribe()
    }

    pub fn shutdown(&self) {
        self.inner.shutdown.send_replace(true);
    }

    #[cfg(test)]
    pub(crate) fn register_contest(
        &self,
        id: ContestId,
        games: Vec<Game>,
        identity: Arc<LiveIdentityIndex>,
        now: time::OffsetDateTime,
    ) {
        self.rebase_contest(id, games, identity, &[], now);
    }

    pub(crate) fn rebase_contest(
        &self,
        id: ContestId,
        games: Vec<Game>,
        identity: Arc<LiveIdentityIndex>,
        feeds: &[coordinator::FeedReplay],
        now: time::OffsetDateTime,
    ) {
        if !self.enabled() {
            return;
        }
        let mut state = self.inner.state.lock().expect("live hub mutex poisoned");
        let locked_at = state
            .registrations
            .get(&id)
            .and_then(|registration| registration.locked_at)
            .or_else(|| slate_locked(&games, now).then_some(now));
        let unchanged = state.registrations.get(&id).is_some_and(|registration| {
            registration.games == games && Arc::ptr_eq(&registration.identity, &identity)
        });
        if unchanged && feeds.is_empty() {
            if let Some(registration) = state.registrations.get_mut(&id) {
                registration.locked_at = locked_at;
            }
            let revision = *state.revisions.entry(id).or_insert(0);
            state
                .senders
                .entry(id)
                .or_insert_with(|| watch::channel(revision).0);
            return;
        }

        let mut reducer = SnapshotReducer::new(id, games.clone(), identity.clone(), now);
        for feed in feeds {
            for event in feed.events() {
                reducer.apply(&event, &feed.slate.games);
            }
            if feed.is_failed() {
                reducer.fail(&feed.slate.games);
            }
        }
        let mut snapshot = reducer.snapshot();
        let changed = state.snapshots.get(&id).is_none_or(|current| {
            current.games != snapshot.games
                || current.stats.players != snapshot.stats.players
                || current.stats.defenses != snapshot.stats.defenses
                || current.game_states != snapshot.game_states
                || current.identity_outcomes != snapshot.identity_outcomes
                || current.latest_error != snapshot.latest_error
                || !Arc::ptr_eq(&current.identity, &snapshot.identity)
        });
        state.registrations.insert(
            id,
            ContestRegistration {
                games,
                identity,
                locked_at,
            },
        );
        state.reducers.insert(id, reducer);
        if !changed {
            return;
        }
        let revision = next_revision(&mut state, id);
        snapshot.revision = revision;
        state.snapshots.insert(id, Arc::new(snapshot));
        state
            .senders
            .entry(id)
            .or_insert_with(|| watch::channel(revision).0)
            .send_replace(revision);
    }

    pub(crate) fn registered_and_locked(&self, id: ContestId) -> bool {
        self.inner
            .state
            .lock()
            .expect("live hub mutex poisoned")
            .registrations
            .get(&id)
            .is_some_and(|registration| registration.locked_at.is_some())
    }

    pub(crate) fn publish_event(
        &self,
        id: ContestId,
        event: &tank01_data::PollEvent,
        feed_games: &[nfl_data::LiveGame],
    ) {
        let mut state = self.inner.state.lock().expect("live hub mutex poisoned");
        let mut snapshot = {
            let Some(reducer) = state.reducers.get_mut(&id) else {
                return;
            };
            if !reducer.accepts_event(event) {
                return;
            }
            reducer.apply(event, feed_games)
        };
        let revision = next_revision(&mut state, id);
        snapshot.revision = revision;
        state.snapshots.insert(id, Arc::new(snapshot));
        if let Some(sender) = state.senders.get(&id) {
            sender.send_replace(revision);
        }
    }

    pub(crate) fn publish_feed_failure(&self, id: ContestId, feed_games: &[nfl_data::LiveGame]) {
        let mut state = self.inner.state.lock().expect("live hub mutex poisoned");
        let mut snapshot = {
            let Some(reducer) = state.reducers.get_mut(&id) else {
                return;
            };
            reducer.fail(feed_games);
            reducer.snapshot()
        };
        let revision = next_revision(&mut state, id);
        snapshot.revision = revision;
        state.snapshots.insert(id, Arc::new(snapshot));
        if let Some(sender) = state.senders.get(&id) {
            sender.send_replace(revision);
        }
    }

    pub(crate) fn notify(&self, id: ContestId) {
        let mut state = self.inner.state.lock().expect("live hub mutex poisoned");
        let revision = next_revision(&mut state, id);
        state
            .senders
            .entry(id)
            .or_insert_with(|| watch::channel(revision).0)
            .send_replace(revision);
    }
}

fn next_revision(state: &mut HubState, id: ContestId) -> u64 {
    let revision = state.revisions.entry(id).or_insert(0);
    *revision = revision.saturating_add(1);
    *revision
}

#[derive(Debug, Clone)]
pub enum ContestScores {
    Upcoming,
    Official(Arc<WeekStats>),
    Provisional(Arc<LiveContestSnapshot>),
    Unavailable,
}

pub fn official_complete_for_slate(games: &[Game], stats: &WeekStats) -> bool {
    let Some((season, week, season_type)) = slate_tuple(games) else {
        return false;
    };
    let mut player_teams = HashSet::new();
    let mut defense_coverage = HashSet::new();
    for game in games {
        let expected = [
            (&game.home_team, &game.away_team),
            (&game.away_team, &game.home_team),
        ];
        for (team, opponent) in expected {
            let defense = stats.defenses.values().filter(|row| {
                row.season == season
                    && row.week == week
                    && row.season_type == season_type
                    && row.gsis_game_id == game.gsis_game_id
                    && row.team == *team
                    && row.opponent == *opponent
            });
            if defense.count() != 1 {
                return false;
            }
            defense_coverage.insert((game.gsis_game_id.as_str(), team.0.as_str()));

            let players = stats.players.values().filter(|row| {
                row.season == season
                    && row.week == week
                    && row.season_type == season_type
                    && row.team == *team
                    && row.opponent.as_ref() == Some(opponent)
            });
            if players.count() == 0 {
                return false;
            }
            player_teams.insert((game.gsis_game_id.as_str(), team.0.as_str()));
        }
    }
    defense_coverage.len() == games.len() * 2 && player_teams.len() == games.len() * 2
}

pub async fn load_contest_scores(
    state: &crate::AppState,
    contest: ContestId,
) -> Result<ContestScores, crate::AppError> {
    let ids = crate::contests::store::game_ids(state.db.reader(), contest).await?;
    if ids.is_empty() {
        return Ok(ContestScores::Unavailable);
    }
    let schedule = state.nfl.games(Season(state.config.season)).await?;
    let games = ids
        .iter()
        .filter_map(|id| schedule.iter().find(|game| game.gsis_game_id == *id))
        .cloned()
        .collect::<Vec<_>>();
    if games.len() != ids.len() {
        return Ok(ContestScores::Unavailable);
    }
    if !slate_locked(&games, state.now()) {
        if games.iter().any(|game| game.kickoff.is_none()) {
            return Ok(ContestScores::Unavailable);
        }
        return Ok(ContestScores::Upcoming);
    }

    if state.config.live_dev_feed
        && let Some(snapshot) = state.live.snapshot(contest)
    {
        return Ok(ContestScores::Provisional(snapshot));
    }

    let Some((season, week, season_type)) = slate_tuple(&games) else {
        return Ok(ContestScores::Unavailable);
    };
    let players = state.nfl.player_week_stats(season, week).await?;
    let defenses = state.nfl.team_week_stats(season, week).await?;
    let expected_games = games
        .iter()
        .map(|game| game.gsis_game_id.as_str())
        .collect::<HashSet<_>>();
    let expected_teams = games
        .iter()
        .flat_map(|game| [game.home_team.0.as_str(), game.away_team.0.as_str()])
        .collect::<HashSet<_>>();

    let mut player_map = HashMap::new();
    for player in players {
        if player.season != season
            || player.week != week
            || player.season_type != season_type
            || !expected_teams.contains(player.team.0.as_str())
            || player
                .opponent
                .as_ref()
                .is_none_or(|opponent| !expected_teams.contains(opponent.0.as_str()))
        {
            continue;
        }
        let matchup = games.iter().any(|game| {
            game.home_team == player.team && player.opponent.as_ref() == Some(&game.away_team)
                || game.away_team == player.team
                    && player.opponent.as_ref() == Some(&game.home_team)
        });
        if !matchup
            || player_map
                .insert(crate::entries::NflPlayerId(player.gsis_id.clone()), player)
                .is_some()
        {
            return Ok(ContestScores::Unavailable);
        }
    }

    let mut defense_map = HashMap::new();
    let mut defense_keys = HashSet::new();
    for defense in defenses {
        if defense.season != season
            || defense.week != week
            || defense.season_type != season_type
            || !expected_games.contains(defense.gsis_game_id.as_str())
            || !expected_teams.contains(defense.team.0.as_str())
        {
            continue;
        }
        let key = (defense.gsis_game_id.clone(), defense.team.0.clone());
        if !defense_keys.insert(key.clone())
            || defense_map.insert(defense.team.clone(), defense).is_some()
        {
            return Ok(ContestScores::Unavailable);
        }
    }

    let stats = WeekStats {
        players: player_map,
        defenses: defense_map,
    };
    if official_complete_for_slate(&games, &stats) {
        return Ok(ContestScores::Official(Arc::new(stats)));
    }
    Ok(state
        .live
        .snapshot(contest)
        .map(ContestScores::Provisional)
        .unwrap_or(ContestScores::Unavailable))
}

pub(crate) async fn stream_eligible(
    state: &crate::AppState,
    contest: ContestId,
    source: &ContestScores,
) -> Result<bool, crate::AppError> {
    if !state.live.enabled() || matches!(source, ContestScores::Official(_)) {
        return Ok(false);
    }
    let reader = state.db.reader();
    let ids = crate::contests::store::game_ids(reader, contest).await?;
    if ids.is_empty() {
        return Ok(false);
    }
    let schedule = state.nfl.games(Season(state.config.season)).await?;
    let games = ids
        .iter()
        .filter_map(|id| {
            schedule
                .iter()
                .find(|game| game.gsis_game_id == id.as_str())
        })
        .cloned()
        .collect::<Vec<_>>();
    if games.len() == ids.len() {
        return Ok(slate_locked(&games, state.now()));
    }
    Ok(state.live.registered_and_locked(contest))
}

pub(crate) fn slate_tuple(games: &[Game]) -> Option<(Season, Week, SeasonType)> {
    let first = games.first()?;
    let tuple = (first.season, first.week, first.season_type);
    games
        .iter()
        .all(|game| (game.season, game.week, game.season_type) == tuple)
        .then_some(tuple)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::factories;
    use nfl_data::{PlayerWeekStats, TeamAbbr as NflTeamAbbr, TeamWeekStats};

    #[test]
    fn official_stats_require_every_slate_team_and_matching_game() {
        let game = factories::game("live", 1);
        let kc = PlayerWeekStats {
            team: NflTeamAbbr("KC".into()),
            opponent: Some(NflTeamAbbr("BUF".into())),
            ..factories::player_stats("kc", 1)
        };
        let buf = PlayerWeekStats {
            gsis_id: "buf".into(),
            team: NflTeamAbbr("BUF".into()),
            opponent: Some(NflTeamAbbr("KC".into())),
            ..factories::player_stats("buf", 1)
        };
        let kc_def = TeamWeekStats {
            gsis_game_id: "live".into(),
            ..factories::defense_stats("KC", "BUF", 1)
        };
        let buf_def = TeamWeekStats {
            gsis_game_id: "live".into(),
            ..factories::defense_stats("BUF", "KC", 1)
        };
        let complete = WeekStats {
            players: HashMap::from([
                (crate::entries::NflPlayerId("kc".into()), kc),
                (crate::entries::NflPlayerId("buf".into()), buf),
            ]),
            defenses: HashMap::from([
                (NflTeamAbbr("KC".into()), kc_def),
                (NflTeamAbbr("BUF".into()), buf_def),
            ]),
        };

        assert!(official_complete_for_slate(
            std::slice::from_ref(&game),
            &complete
        ));

        let mut missing = complete.clone();
        missing.defenses.remove(&NflTeamAbbr("BUF".into()));
        assert!(!official_complete_for_slate(
            std::slice::from_ref(&game),
            &missing
        ));
    }
}
