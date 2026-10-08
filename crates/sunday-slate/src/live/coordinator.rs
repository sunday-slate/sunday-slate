use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ::time::{Date, OffsetDateTime};
use nfl_data::{
    Game, InjuryEntry, InjuryProvider, LiveGame, LiveScoreProvider, LiveSlate, ProviderOutcome,
    Season, SeasonType, TeamAbbr, Week,
};
use tank01_data::{PollEvent, SlatePoller};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::{self, MissedTickBehavior};

use crate::contests::{ContestId, store};
use crate::live::LiveContestHub;
use crate::live::identity::LiveIdentityIndex;
use crate::live::snapshot::live_game;
use crate::player_identity::normalize_team;
use crate::{AppError, AppState};

const RECONCILE_INTERVAL: Duration = Duration::from_secs(20);
const STARTUP_RECOVERY: ::time::Duration = ::time::Duration::hours(12);

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct FeedKey {
    pub(crate) date: Date,
    pub(crate) provider_game_ids: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FeedTerminal {
    Running,
    Completed,
    Failed,
}

#[derive(Clone, Default)]
struct BoxReplay {
    latest_player_section: Option<Arc<PollEvent>>,
    latest_home_defense: Option<Arc<PollEvent>>,
    latest_away_defense: Option<Arc<PollEvent>>,
    latest_success: Option<Arc<PollEvent>>,
    latest_response: Option<Arc<PollEvent>>,
    latest_failure: Option<Arc<PollEvent>>,
}

#[derive(Clone)]
pub(crate) struct FeedReplay {
    pub(crate) slate: LiveSlate,
    scoreboard_success: Option<Arc<PollEvent>>,
    scoreboard_response: Option<Arc<PollEvent>>,
    boxes: HashMap<String, BoxReplay>,
    terminal: FeedTerminal,
}

struct InjuryWindow {
    first: OffsetDateTime,
    last: OffsetDateTime,
}

struct InjuryTarget {
    window: Option<InjuryWindow>,
    teams: Vec<TeamAbbr>,
    contests: BTreeSet<ContestId>,
}

impl FeedReplay {
    fn new(slate: LiveSlate) -> Self {
        Self {
            slate,
            scoreboard_success: None,
            scoreboard_response: None,
            boxes: HashMap::new(),
            terminal: FeedTerminal::Running,
        }
    }

    fn accepts_box(&self, game: &LiveGame) -> bool {
        self.slate.games.iter().any(|candidate| {
            candidate.gsis_game_id == game.gsis_game_id
                && candidate.provider_game_id == game.provider_game_id
        })
    }

    fn ingest(&mut self, event: Arc<PollEvent>) {
        match event.as_ref() {
            PollEvent::Scoreboard { response, .. } => {
                replace_latest(&mut self.scoreboard_response, &event);
                if matches!(&response.outcome, ProviderOutcome::Value(_)) {
                    replace_latest(&mut self.scoreboard_success, &event);
                }
            }
            PollEvent::BoxScore { game, response, .. } if self.accepts_box(game) => {
                let replay = self.boxes.entry(game.gsis_game_id.clone()).or_default();
                replace_latest(&mut replay.latest_response, &event);
                if let ProviderOutcome::Value(snapshot) = &response.outcome {
                    replace_latest(&mut replay.latest_success, &event);
                    if snapshot.players.is_some() {
                        replace_latest(&mut replay.latest_player_section, &event);
                    }
                    if let Some(defenses) = &snapshot.defenses {
                        if defenses.iter().any(|defense| {
                            normalize_team(&defense.team.0) == normalize_team(&game.home_team.0)
                                && defense.dst_present
                                && defense.team_stats_present
                        }) {
                            replace_latest(&mut replay.latest_home_defense, &event);
                        }
                        if defenses.iter().any(|defense| {
                            normalize_team(&defense.team.0) == normalize_team(&game.away_team.0)
                                && defense.dst_present
                                && defense.team_stats_present
                        }) {
                            replace_latest(&mut replay.latest_away_defense, &event);
                        }
                    }
                } else {
                    replace_latest(&mut replay.latest_failure, &event);
                }
            }
            PollEvent::RunFinished { .. } => {
                self.terminal = FeedTerminal::Completed;
            }
            PollEvent::BoxScore { .. } => {}
        }
    }

    pub(crate) fn events(&self) -> Vec<Arc<PollEvent>> {
        let mut events = BTreeMap::<u64, Arc<PollEvent>>::new();
        for event in [
            self.scoreboard_success.as_ref(),
            self.scoreboard_response.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            events.insert(event_sequence(event), Arc::clone(event));
        }
        for replay in self.boxes.values() {
            for event in [
                replay.latest_player_section.as_ref(),
                replay.latest_home_defense.as_ref(),
                replay.latest_away_defense.as_ref(),
                replay.latest_success.as_ref(),
                replay.latest_response.as_ref(),
                replay.latest_failure.as_ref(),
            ]
            .into_iter()
            .flatten()
            {
                events.insert(event_sequence(event), Arc::clone(event));
            }
        }
        events.into_values().collect()
    }
    pub(crate) fn is_failed(&self) -> bool {
        self.terminal == FeedTerminal::Failed
    }
}

fn replace_latest(slot: &mut Option<Arc<PollEvent>>, event: &Arc<PollEvent>) {
    if slot
        .as_ref()
        .is_none_or(|current| event_sequence(current) < event_sequence(event))
    {
        *slot = Some(Arc::clone(event));
    }
}

fn event_sequence(event: &PollEvent) -> u64 {
    match event {
        PollEvent::Scoreboard { sequence, .. }
        | PollEvent::BoxScore { sequence, .. }
        | PollEvent::RunFinished { sequence, .. } => *sequence,
    }
}
struct FeedState {
    replay: FeedReplay,
    contests: HashSet<ContestId>,
}

struct FeedRunner {
    state: Arc<Mutex<FeedState>>,
    handle: JoinHandle<()>,
}

#[derive(Default)]
struct CoordinatorState {
    feeds: HashMap<FeedKey, FeedRunner>,
    replays: HashMap<FeedKey, FeedReplay>,
    started: HashSet<ContestId>,
    identity: HashMap<(Season, Week, SeasonType, Vec<String>), Arc<LiveIdentityIndex>>,
    revision: Option<nfl_data::DataRevision>,
}

pub async fn run_coordinator(state: AppState, provider: tank01_data::Tank01Client) {
    run_coordinator_with_provider(state, provider).await;
}

pub async fn run_coordinator_with_provider<P>(state: AppState, provider: P)
where
    P: LiveScoreProvider + InjuryProvider + Clone + 'static,
{
    let mut shutdown = state.live.shutdown_receiver();
    let mut reconcile = time::interval(RECONCILE_INTERVAL);
    reconcile.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut coordinator = CoordinatorState::default();
    if *shutdown.borrow() {
        return;
    }
    if let Err(error) = reconcile_once(&state, &provider, &mut coordinator).await {
        tracing::warn!(error = ?error, "initial live feed reconciliation failed");
    }
    if !*shutdown.borrow() {
        let _ = reconcile.tick().await;
    }
    loop {
        if *shutdown.borrow() {
            break;
        }
        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break;
                }
            }
            _ = reconcile.tick() => {
                if *shutdown.borrow() {
                    break;
                }
                if let Err(error) = reconcile_once(&state, &provider, &mut coordinator).await {
                    tracing::warn!(error = ?error, "live feed reconciliation failed");
                }
            }
        }
    }

    let handles = coordinator
        .feeds
        .drain()
        .map(|(_, runner)| runner.handle)
        .collect::<Vec<_>>();
    for handle in handles {
        handle.abort();
        let _ = handle.await;
    }
}

async fn reconcile_once<P>(
    state: &AppState,
    provider: &P,
    coordinator: &mut CoordinatorState,
) -> Result<(), AppError>
where
    P: LiveScoreProvider + InjuryProvider + Clone + 'static,
{
    let now = state.now_eastern();
    let schedule = state.nfl.games(Season(state.config.season)).await?;
    let revision = state.nfl.data_revision().await?;
    if coordinator.revision.as_ref() != Some(&revision) {
        coordinator.identity.clear();
        coordinator.revision = Some(revision);
    }

    let materialized = store::materialized_games(state.db.reader()).await?;
    let contests = store::all(state.db.reader()).await?;
    let mut discovered = BTreeMap::<FeedKey, (LiveSlate, BTreeSet<ContestId>)>::new();
    let mut contest_updates =
        BTreeMap::<ContestId, (Vec<Game>, Arc<LiveIdentityIndex>, Vec<FeedKey>)>::new();
    let mut targets = HashMap::<crate::injuries::SlateKey, InjuryTarget>::new();

    for contest in contests {
        let Some(ids) = materialized.get(&contest.id) else {
            continue;
        };
        let games = resolve_games(ids, &schedule);
        if games.len() != ids.len() || games.is_empty() {
            continue;
        }
        let first = games.iter().filter_map(Game::kickoff_eastern).min();
        let last = games.iter().filter_map(Game::kickoff_eastern).max();
        let started = coordinator.started.contains(&contest.id)
            || coordinator.feeds.values().any(|runner| {
                runner
                    .state
                    .lock()
                    .expect("feed mutex poisoned")
                    .contests
                    .contains(&contest.id)
            });

        let tuple = (games[0].season, games[0].week, games[0].season_type);
        if !games
            .iter()
            .all(|game| (game.season, game.week, game.season_type) == tuple)
        {
            continue;
        }

        let mut teams = games
            .iter()
            .flat_map(|game| [game.home_team.clone(), game.away_team.clone()])
            .collect::<Vec<_>>();
        teams.sort_by(|a, b| a.0.cmp(&b.0));
        teams.dedup_by(|a, b| a.0 == b.0);

        if !started
            && first
                .zip(last)
                .is_some_and(|(first, last)| first > now || last < now - STARTUP_RECOVERY)
        {
            if last.is_some_and(|last| last < now - STARTUP_RECOVERY) {
                continue;
            }
            capture_injury_target(&mut targets, &games, &tuple, &teams, contest.id);
            continue;
        }

        if matches!(
            crate::live::load_contest_scores(state, contest.id).await?,
            crate::live::ContestScores::Official(_)
        ) {
            state.live.notify(contest.id);
            continue;
        }

        capture_injury_target(&mut targets, &games, &tuple, &teams, contest.id);

        let identity_key = (
            tuple.0,
            tuple.1,
            tuple.2,
            teams.iter().map(|team| team.0.clone()).collect::<Vec<_>>(),
        );
        let identity = if let Some(index) = coordinator.identity.get(&identity_key) {
            index.clone()
        } else {
            let index =
                Arc::new(LiveIdentityIndex::build(&state.nfl, tuple.0, tuple.1, &teams).await?);
            coordinator.identity.insert(identity_key, index.clone());
            index
        };

        let mut feed_keys = Vec::new();
        let mut by_date = BTreeMap::<Date, Vec<&Game>>::new();
        for game in &games {
            let Some(et) = game.kickoff_eastern() else {
                continue;
            };
            by_date.entry(et.date()).or_default().push(game);
        }
        for (date, date_games) in by_date {
            let Some(earliest) = date_games
                .iter()
                .filter_map(|game| game.kickoff_eastern())
                .min()
            else {
                continue;
            };
            if earliest > now {
                continue;
            }
            let live_games = date_games
                .into_iter()
                .filter_map(live_game)
                .collect::<Vec<_>>();
            if live_games.is_empty() {
                continue;
            }
            let mut provider_ids = live_games
                .iter()
                .map(|game| game.provider_game_id.clone())
                .collect::<Vec<_>>();
            provider_ids.sort();
            provider_ids.dedup();
            let feed_key = FeedKey {
                date,
                provider_game_ids: provider_ids,
            };
            discovered
                .entry(feed_key.clone())
                .or_insert_with(|| {
                    (
                        LiveSlate {
                            date,
                            games: live_games,
                        },
                        BTreeSet::new(),
                    )
                })
                .1
                .insert(contest.id);
            feed_keys.push(feed_key);
        }
        feed_keys.sort();
        contest_updates.insert(contest.id, (games, identity, feed_keys));
    }

    coordinator
        .replays
        .retain(|key, _| discovered.contains_key(key));
    let retired = coordinator
        .feeds
        .iter()
        .filter_map(|(key, runner)| {
            let feed = runner.state.lock().expect("feed mutex poisoned");
            let stopped =
                runner.handle.is_finished() || feed.replay.terminal == FeedTerminal::Failed;
            let orphaned = !discovered.contains_key(key);
            (orphaned || stopped).then(|| {
                (
                    key.clone(),
                    discovered.contains_key(key).then(|| feed.replay.clone()),
                )
            })
        })
        .collect::<Vec<_>>();
    for (key, replay) in retired {
        if let Some(runner) = coordinator.feeds.remove(&key) {
            stop_feed(runner).await;
        }
        if let Some(mut replay) = replay {
            if replay.terminal == FeedTerminal::Running {
                replay.terminal = FeedTerminal::Failed;
            }
            coordinator.replays.insert(key, replay);
        }
    }

    for (key, (_, contests)) in &discovered {
        let Some(existing) = coordinator.feeds.get(key) else {
            continue;
        };
        let mut feed = existing.state.lock().expect("feed mutex poisoned");
        feed.contests = contests.iter().copied().collect();
    }
    for (key, (slate, contests)) in &discovered {
        if coordinator.feeds.contains_key(key) || coordinator.replays.contains_key(key) {
            continue;
        }
        let replay = FeedReplay::new(slate.clone());
        let feed_state = Arc::new(Mutex::new(FeedState {
            replay,
            contests: contests.iter().copied().collect(),
        }));
        let state_for_task = Arc::clone(&feed_state);
        let events_hub = state.live.clone();
        let provider = provider.clone();
        let slate = slate.clone();
        let handle = tokio::spawn(async move {
            run_feed(provider, slate, events_hub, state_for_task).await;
        });
        coordinator.feeds.insert(
            key.clone(),
            FeedRunner {
                state: feed_state,
                handle,
            },
        );
        for contest in contests {
            coordinator.started.insert(*contest);
        }
    }

    for (key, runner) in &coordinator.feeds {
        let own = discovered
            .get(key)
            .map(|(_, contests)| contests)
            .cloned()
            .unwrap_or_default();
        runner
            .state
            .lock()
            .expect("feed mutex poisoned")
            .contests
            .retain(|contest| own.contains(contest));
    }

    for (contest, (games, identity, feed_keys)) in contest_updates {
        let mut guards = Vec::with_capacity(feed_keys.len());
        let mut replays = Vec::with_capacity(feed_keys.len());
        for key in &feed_keys {
            if let Some(runner) = coordinator.feeds.get(key) {
                guards.push(runner.state.lock().expect("feed mutex poisoned"));
                replays.push(
                    guards
                        .last()
                        .expect("newly acquired feed guard")
                        .replay
                        .clone(),
                );
            } else if let Some(replay) = coordinator.replays.get(key) {
                replays.push(replay.clone());
            }
        }
        state
            .live
            .rebase_contest(contest, games, identity, &replays, now);
    }

    for contest in materialized.keys().copied() {
        state.live.notify(contest);
    }

    refresh_injuries(state, provider, coordinator, &targets, now).await?;
    Ok(())
}

fn injury_ttl(now: OffsetDateTime, window: Option<&InjuryWindow>) -> ::time::Duration {
    const SHARP: ::time::Duration = ::time::Duration::minutes(5);
    const IDLE: ::time::Duration = ::time::Duration::minutes(30);
    const MARGIN: ::time::Duration = ::time::Duration::hours(4);
    let Some(window) = window else {
        return IDLE;
    };
    if (window.first - MARGIN) <= now && now <= (window.last + MARGIN) {
        SHARP
    } else {
        IDLE
    }
}

fn capture_injury_target(
    targets: &mut HashMap<crate::injuries::SlateKey, InjuryTarget>,
    games: &[Game],
    tuple: &crate::injuries::SlateKey,
    teams: &[TeamAbbr],
    contest: ContestId,
) {
    let entry = targets.entry(*tuple).or_insert_with(|| InjuryTarget {
        window: None,
        teams: teams.to_vec(),
        contests: BTreeSet::new(),
    });
    entry.contests.insert(contest);
    for game in games {
        if let Some(kickoff) = game.kickoff_eastern() {
            let window = entry.window.get_or_insert(InjuryWindow {
                first: kickoff,
                last: kickoff,
            });
            window.first = window.first.min(kickoff);
            window.last = window.last.max(kickoff);
        }
    }
}

async fn refresh_injuries<P>(
    state: &AppState,
    provider: &P,
    coordinator: &mut CoordinatorState,
    targets: &HashMap<crate::injuries::SlateKey, InjuryTarget>,
    now: OffsetDateTime,
) -> Result<(), AppError>
where
    P: InjuryProvider,
{
    let stale: Vec<(&crate::injuries::SlateKey, &InjuryTarget)> = targets
        .iter()
        .filter(|(key, target)| {
            state
                .injuries
                .needs_refresh(key, now, injury_ttl(now, target.window.as_ref()))
        })
        .collect();
    if stale.is_empty() {
        tracing::debug!(
            targets = targets.len(),
            "injury refresh: nothing stale (no materialized slates, all fresh, or off-window)"
        );
        return Ok(());
    }
    tracing::info!(
        slates = stale.len(),
        keys = %stale.iter().map(|(key, _)| format!("{:?}", key)).collect::<Vec<_>>().join(", "),
        "injury refresh: fetching"
    );

    let response = provider.current_injuries().await;
    match &response.outcome {
        ProviderOutcome::Value(report) => {
            for (key, target) in stale {
                let identity_key = (
                    key.0,
                    key.1,
                    key.2,
                    target
                        .teams
                        .iter()
                        .map(|team| team.0.clone())
                        .collect::<Vec<_>>(),
                );
                let index = match coordinator.identity.get(&identity_key) {
                    Some(index) => index.clone(),
                    None => {
                        let index = Arc::new(
                            LiveIdentityIndex::build(&state.nfl, key.0, key.1, &target.teams)
                                .await?,
                        );
                        coordinator.identity.insert(identity_key, index.clone());
                        index
                    }
                };
                let by_gsis = report
                    .entries
                    .iter()
                    .filter_map(|entry: &InjuryEntry| {
                        match index.resolve(
                            entry.espn_id.as_ref(),
                            Some(&entry.name),
                            entry.team.as_ref(),
                            entry.position.as_deref(),
                        ) {
                            crate::live::identity::IdentityMatch::Matched(gsis) => {
                                Some((gsis, entry.designation))
                            }
                            _ => None,
                        }
                    })
                    .collect::<HashMap<_, _>>();
                let matched = by_gsis.len();
                state.injuries.publish(
                    *key,
                    crate::injuries::SlateInjuries {
                        by_gsis,
                        report_date: report.report_date,
                        attempted_at: Some(now),
                    },
                );
                tracing::info!(
                    season = %key.0.0,
                    week = %key.1.0,
                    report_date = ?report.report_date,
                    entries = report.entries.len(),
                    matched,
                    "injury refresh: report published"
                );
                notify_contests(state, &target.contests);
            }
        }
        ProviderOutcome::Pregame { message } => {
            for (key, target) in stale {
                tracing::info!(
                    season = %key.0.0,
                    week = %key.1.0,
                    message = %message,
                    "injury refresh: provider had no report"
                );
                state.injuries.publish(
                    *key,
                    crate::injuries::SlateInjuries {
                        attempted_at: Some(now),
                        ..Default::default()
                    },
                );
                notify_contests(state, &target.contests);
            }
        }
        ProviderOutcome::Error(error) => {
            tracing::warn!(error = %error, "injury refresh failed; keeping prior report");
            for (key, target) in stale {
                let mut previous = state.injuries.report(key).unwrap_or_default();
                previous.attempted_at = Some(now);
                state.injuries.publish(*key, previous);
                notify_contests(state, &target.contests);
            }
        }
    }
    Ok(())
}

fn notify_contests(state: &AppState, contests: &BTreeSet<ContestId>) {
    for contest in contests {
        state.live.notify(*contest);
    }
}
async fn stop_feed(runner: FeedRunner) {
    runner.handle.abort();
    let _ = runner.handle.await;
}

enum FeedStep {
    Poller(Result<(), tank01_data::PollerError>),
    Event(Box<PollEvent>),
    EventsClosed,
}

async fn run_feed<P>(
    provider: P,
    slate: LiveSlate,
    hub: Arc<LiveContestHub>,
    feed_state: Arc<Mutex<FeedState>>,
) where
    P: LiveScoreProvider + Clone + 'static,
{
    let feed_games = slate.games.clone();
    let (sender, mut receiver) = mpsc::channel(256);
    let poller = match SlatePoller::new(provider, slate, sender) {
        Ok(poller) => Box::pin(poller.run()),
        Err(error) => {
            tracing::warn!(error = ?error, "live feed rejected");
            let mut feed = feed_state.lock().expect("feed mutex poisoned");
            feed.replay.terminal = FeedTerminal::Failed;
            for contest in feed.contests.iter().copied() {
                hub.publish_feed_failure(contest, &feed_games);
            }
            return;
        }
    };
    let mut poller = Some(poller);
    let mut poller_result = None;

    loop {
        let step = if let Some(poller_future) = poller.as_mut() {
            tokio::select! {
                result = poller_future => FeedStep::Poller(result),
                event = receiver.recv() => match event {
                    Some(event) => FeedStep::Event(Box::new(event)),
                    None => FeedStep::EventsClosed,
                },
            }
        } else {
            match receiver.recv().await {
                Some(event) => FeedStep::Event(Box::new(event)),
                None => FeedStep::EventsClosed,
            }
        };
        match step {
            FeedStep::Poller(result) => {
                poller = None;
                poller_result = Some(result);
            }
            FeedStep::Event(event) => ingest_and_publish(&feed_state, &hub, *event),
            FeedStep::EventsClosed => {
                if poller.is_some() {
                    poller = None;
                    poller_result = Some(Err(tank01_data::PollerError::SinkClosed));
                } else {
                    break;
                }
            }
        }
    }

    let terminal = match poller_result {
        Some(Ok(())) => FeedTerminal::Completed,
        Some(Err(error)) => {
            tracing::warn!(error = ?error, "live feed stopped");
            FeedTerminal::Failed
        }
        None => FeedTerminal::Failed,
    };
    let mut feed = feed_state.lock().expect("feed mutex poisoned");
    feed.replay.terminal = terminal;
    if terminal == FeedTerminal::Failed {
        for contest in feed.contests.iter().copied() {
            hub.publish_feed_failure(contest, &feed.replay.slate.games);
        }
    }
}

fn ingest_and_publish(
    feed_state: &Arc<Mutex<FeedState>>,
    hub: &Arc<LiveContestHub>,
    event: PollEvent,
) {
    let event = Arc::new(event);
    let mut feed = feed_state.lock().expect("feed mutex poisoned");
    feed.replay.ingest(Arc::clone(&event));
    for contest in feed.contests.iter().copied() {
        hub.publish_event(contest, &event, &feed.replay.slate.games);
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use nfl_data::{
        EspnPlayerId, Game, InjuryDesignation, InjuryEntry, InjuryProvider, InjuryReport, LiveGame,
        LiveGamePhase, LiveGameSnapshot, LiveScoreProvider, LiveScoreboard, LiveScoreboardGame,
        ProviderOutcome, ProviderResponse, Season, SeasonType, TeamAbbr, Week,
    };
    use sqlx::SqlitePool;
    use time::OffsetDateTime;
    use time::macros::datetime;
    use tokio::sync::Notify;

    use super::{
        CoordinatorState, FeedKey, FeedTerminal, injury_ttl, reconcile_once,
        run_coordinator_with_provider,
    };
    use crate::contests::ContestId;
    use crate::live::{ContestScores, live_game, load_contest_scores, provider_game_id};
    use crate::tests::TestApp;
    use crate::tests::factories;

    const KICKOFF: OffsetDateTime = datetime!(2025-09-07 17:00 UTC);

    struct FakeProviderState {
        scoreboard_calls: AtomicUsize,
        box_score_calls: AtomicUsize,
        injury_calls: AtomicUsize,
        injuries: Option<InjuryReport>,
        notify: Notify,
    }

    #[derive(Clone)]
    struct FakeProvider {
        game: LiveGame,
        state: Arc<FakeProviderState>,
    }

    impl FakeProvider {
        fn for_game(game: &Game) -> Self {
            Self {
                game: live_game(game).expect("fixture kickoff"),
                state: Arc::new(FakeProviderState {
                    scoreboard_calls: AtomicUsize::new(0),
                    box_score_calls: AtomicUsize::new(0),
                    injury_calls: AtomicUsize::new(0),
                    injuries: None,
                    notify: Notify::new(),
                }),
            }
        }

        fn with_injuries(mut self, report: InjuryReport) -> Self {
            let state = Arc::get_mut(&mut self.state).expect("fresh fake provider");
            state.injuries = Some(report);
            self
        }
    }

    impl LiveScoreProvider for FakeProvider {
        async fn scoreboard(&self, _date: time::Date) -> ProviderResponse<LiveScoreboard> {
            self.state.scoreboard_calls.fetch_add(1, Ordering::SeqCst);
            self.state.notify.notify_one();
            response(ProviderOutcome::Value(LiveScoreboard {
                games: vec![LiveScoreboardGame {
                    provider_game_id: self.game.provider_game_id.clone(),
                    phase: LiveGamePhase::InProgress,
                    period: Some("2".into()),
                    clock: Some("08:00".into()),
                    home_team: self.game.home_team.clone(),
                    away_team: self.game.away_team.clone(),
                    home_score: Some(7),
                    away_score: Some(3),
                }],
            }))
        }

        async fn box_score(
            &self,
            game: &LiveGame,
            _play_by_play: bool,
        ) -> ProviderResponse<LiveGameSnapshot> {
            self.state.box_score_calls.fetch_add(1, Ordering::SeqCst);
            self.state.notify.notify_one();
            response(ProviderOutcome::Value(LiveGameSnapshot {
                game: game.clone(),
                observed_at: KICKOFF + time::Duration::seconds(30),
                phase: LiveGamePhase::InProgress,
                period: Some("2".into()),
                clock: Some("08:00".into()),
                home_score: Some(7),
                away_score: Some(3),
                players: Some(Vec::new()),
                defenses: Some(Vec::new()),
            }))
        }
    }

    impl InjuryProvider for FakeProvider {
        async fn current_injuries(&self) -> ProviderResponse<InjuryReport> {
            self.state.injury_calls.fetch_add(1, Ordering::SeqCst);
            self.state.notify.notify_one();
            match &self.state.injuries {
                Some(report) => response(ProviderOutcome::Value(report.clone())),
                None => response(ProviderOutcome::Pregame {
                    message: "test provider".into(),
                }),
            }
        }
    }

    fn response<T>(outcome: ProviderOutcome<T>) -> ProviderResponse<T> {
        ProviderResponse {
            requested_at: KICKOFF,
            received_at: KICKOFF,

            elapsed_ms: 1,
            http_status: Some(200),
            headers: Default::default(),
            raw_body: None,
            outcome,
        }
    }

    async fn wait_for(
        state: &FakeProviderState,
        counter: &AtomicUsize,
        task: &tokio::task::JoinHandle<()>,
    ) {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if counter.load(Ordering::SeqCst) > 0 {
                    return;
                }
                assert!(
                    !task.is_finished(),
                    "coordinator stopped before provider call"
                );
                let notified = state.notify.notified();
                if counter.load(Ordering::SeqCst) > 0 {
                    return;
                }
                notified.await;
            }
        })
        .await
        .expect("provider call did not arrive");
    }

    async fn app_with_slate(
        pool: &SqlitePool,
        now: OffsetDateTime,
        games: &[Game],
        name: &str,
    ) -> (TestApp, ContestId) {
        let app = TestApp::from_pool_at(pool.clone(), now).await;
        let ids = games
            .iter()
            .map(|game| game.gsis_game_id.as_str())
            .collect::<Vec<_>>();
        let contest = factories::contest_with_games(pool, name, &ids).await;
        app.nfl.seed_for_test(&[], games).await.unwrap();
        (app, ContestId(contest))
    }

    async fn mocked_nfl_host() -> (TestApp, wiremock::MockServer, tempfile::TempDir) {
        let (nfl, server, dir) = crate::tests::nfl::mocked_nfl().await;
        let app = TestApp::new_with_nfl(nfl).await;
        (app, server, dir)
    }

    async fn seed_alpha_runner(app: &TestApp) {
        let mut alpha = factories::player("00-A", "Alpha Runner");
        alpha.position = Some("RB".into());
        alpha.espn_id = Some("injury-alpha".into());
        app.nfl.seed_for_test(&[alpha], &[]).await.unwrap();
    }

    fn alpha_injury_report() -> InjuryReport {
        InjuryReport {
            report_date: Some(time::macros::date!(2025 - 09 - 06)),
            entries: vec![InjuryEntry {
                espn_id: Some(EspnPlayerId("injury-alpha".into())),
                name: "Alpha Runner".into(),
                team: Some(TeamAbbr("KC".into())),
                position: Some("RB".into()),
                designation: InjuryDesignation::Questionable,
            }],
        }
    }

    const REVISION_CSV: &str = "game_id,season,week,game_type,gameday,gametime,home_team,away_team,home_score,away_score\n2025_01_BUF_KC,2025,1,REG,2025-09-07,13:00,KC,BUF,,\n";

    async fn await_revision_change(
        nfl: &nfl_data::NflData,
        previous: &nfl_data::DataRevision,
    ) -> nfl_data::DataRevision {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let current = nfl.data_revision().await.unwrap();
                if &current != previous {
                    return current;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("mocked refresh changes the revision")
    }

    async fn abort_feeds(coordinator: &mut CoordinatorState) {
        for (_, runner) in coordinator.feeds.drain() {
            runner.handle.abort();
            let _ = runner.handle.await;
        }
    }

    #[sqlx::test]
    async fn coordinator_reuses_identity_for_equal_revision_and_rebuilds_after_sync(
        _pool: SqlitePool,
    ) {
        let (app, server, _dir) = mocked_nfl_host().await;
        crate::tests::nfl::mount_schedule_release(&server, "2026-01-01T00:00:00Z", REVISION_CSV)
            .await;
        app.login_admin().await;
        app.post("/nfl-data-admin/nflverse", "")
            .await
            .assert_status_ok();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if !app.nfl.games(Season(2025)).await.unwrap().is_empty() {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("initial mocked schedule sync completes");
        let first_revision = app.nfl.data_revision().await.unwrap();

        let games = app.nfl.games(Season(2025)).await.unwrap();
        let game = games.first().expect("mocked schedule game").clone();
        let contest = factories::contest_with_games(
            &app.pool,
            "Revision identity",
            &[game.gsis_game_id.as_str()],
        )
        .await;
        let contest = ContestId(contest);
        let provider = FakeProvider::for_game(&game);
        let mut coordinator = CoordinatorState::default();
        coordinator.started.insert(contest);
        reconcile_once(&app.state, &provider, &mut coordinator)
            .await
            .unwrap();
        let cached = coordinator
            .identity
            .values()
            .next()
            .expect("identity index cached")
            .clone();

        reconcile_once(&app.state, &provider, &mut coordinator)
            .await
            .unwrap();
        let reused = coordinator.identity.values().next().unwrap();
        assert!(Arc::ptr_eq(&cached, reused));

        tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
        crate::tests::nfl::mount_schedule_release(&server, "2026-01-02T00:00:00Z", REVISION_CSV)
            .await;
        app.post("/nfl-data-admin/nflverse", "")
            .await
            .assert_status_ok();
        let _updated_revision = await_revision_change(&app.nfl, &first_revision).await;
        reconcile_once(&app.state, &provider, &mut coordinator)
            .await
            .unwrap();
        let rebuilt = coordinator.identity.values().next().unwrap();
        assert!(!Arc::ptr_eq(&cached, rebuilt));

        abort_feeds(&mut coordinator).await;
        app.state.live.shutdown();
    }

    #[sqlx::test]
    async fn coordinator_revision_errors_follow_reconciliation_error_path(_pool: SqlitePool) {
        let (app, _server, dir) = mocked_nfl_host().await;
        let mut game = factories::game_at("2025_01_BUF_KC", 1, KICKOFF);
        game.kickoff = None;
        let contest = factories::contest_with_games(
            &app.pool,
            "Revision error",
            &[game.gsis_game_id.as_str()],
        )
        .await;
        app.nfl
            .seed_for_test(&[], std::slice::from_ref(&game))
            .await
            .unwrap();
        let provider = FakeProvider::for_game(&factories::game_at("2025_01_BUF_KC", 1, KICKOFF));
        let mut coordinator = CoordinatorState::default();
        coordinator.started.insert(ContestId(contest));
        reconcile_once(&app.state, &provider, &mut coordinator)
            .await
            .unwrap();
        let cached_revision = coordinator.revision.clone().expect("revision stored");
        let cached_index = coordinator.identity.values().next().unwrap().clone();

        let db_url = format!("sqlite://{}/cache.db", dir.path().display());
        let fault = sqlx::SqlitePool::connect(&db_url).await.unwrap();
        sqlx::query("DROP TABLE sync_state")
            .execute(&fault)
            .await
            .unwrap();
        assert!(
            reconcile_once(&app.state, &provider, &mut coordinator)
                .await
                .is_err()
        );
        assert_eq!(coordinator.revision.as_ref(), Some(&cached_revision));
        assert!(Arc::ptr_eq(
            &cached_index,
            coordinator.identity.values().next().unwrap()
        ));
        fault.close().await;
        app.state.live.shutdown();
    }

    #[sqlx::test]
    async fn coordinator_polls_started_feed_and_publishes_snapshot(pool: SqlitePool) {
        let game = factories::game_at("2025_01_BUF_KC", 1, KICKOFF);
        let (app, contest) = app_with_slate(
            &pool,
            KICKOFF + time::Duration::hours(1),
            std::slice::from_ref(&game),
            "Started",
        )
        .await;
        let provider = FakeProvider::for_game(&game);
        let mut changes = app.state.live.subscribe(contest);
        let coordinator = tokio::spawn(run_coordinator_with_provider(
            app.state.clone(),
            provider.clone(),
        ));

        wait_for(
            &provider.state,
            &provider.state.scoreboard_calls,
            &coordinator,
        )
        .await;
        wait_for(
            &provider.state,
            &provider.state.box_score_calls,
            &coordinator,
        )
        .await;
        let snapshot = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if let Some(snapshot) = app.state.live.snapshot(contest) {
                    break snapshot;
                }
                changes.changed().await.expect("live hub stays open");
            }
        })
        .await
        .expect("box-score event reaches the live hub");
        assert_eq!(
            snapshot
                .game_states
                .get(&game.gsis_game_id)
                .expect("game snapshot")
                .phase,
            LiveGamePhase::InProgress
        );

        app.state.live.shutdown();
        coordinator.await.unwrap();
    }

    #[sqlx::test]
    async fn coordinator_can_restart_with_a_fresh_hub(pool: SqlitePool) {
        let game = factories::game_at("2025_01_BUF_KC", 1, KICKOFF);
        let (app, _) = app_with_slate(
            &pool,
            KICKOFF + time::Duration::hours(1),
            std::slice::from_ref(&game),
            "Restarted",
        )
        .await;
        let provider = FakeProvider::for_game(&game);
        let first = tokio::spawn(run_coordinator_with_provider(
            app.state.clone(),
            provider.clone(),
        ));
        wait_for(&provider.state, &provider.state.scoreboard_calls, &first).await;
        app.state.live.shutdown();
        first.await.unwrap();

        let (fresh_app, fresh_contest) = app_with_slate(
            &pool,
            KICKOFF + time::Duration::hours(1),
            std::slice::from_ref(&game),
            "Restarted Fresh",
        )
        .await;
        let fresh_provider = FakeProvider::for_game(&game);
        let second = tokio::spawn(run_coordinator_with_provider(
            fresh_app.state.clone(),
            fresh_provider.clone(),
        ));
        wait_for(
            &fresh_provider.state,
            &fresh_provider.state.box_score_calls,
            &second,
        )
        .await;
        assert!(fresh_app.state.live.snapshot(fresh_contest).is_some());
        fresh_app.state.live.shutdown();
        second.await.unwrap();
    }

    #[sqlx::test]
    async fn coordinator_skips_future_slate(pool: SqlitePool) {
        let game = factories::game_at("2025_01_BUF_KC", 1, KICKOFF);
        let (app, contest) = app_with_slate(
            &pool,
            KICKOFF - time::Duration::hours(1),
            std::slice::from_ref(&game),
            "Future",
        )
        .await;
        let provider = FakeProvider::for_game(&game);
        let mut coordinator = CoordinatorState::default();

        reconcile_once(&app.state, &provider, &mut coordinator)
            .await
            .unwrap();

        assert!(coordinator.feeds.is_empty());
        assert!(app.state.live.snapshot(contest).is_none());
        app.state.live.shutdown();
    }

    #[sqlx::test]
    async fn coordinator_targets_future_slate_for_injuries(pool: SqlitePool) {
        let game = factories::game_at("2025_01_BUF_KC", 1, KICKOFF);
        let (app, contest) = app_with_slate(
            &pool,
            KICKOFF - time::Duration::hours(1),
            std::slice::from_ref(&game),
            "Future Injuries",
        )
        .await;
        seed_alpha_runner(&app).await;
        let provider = FakeProvider::for_game(&game).with_injuries(alpha_injury_report());
        let mut coordinator = CoordinatorState::default();

        reconcile_once(&app.state, &provider, &mut coordinator)
            .await
            .unwrap();

        assert_eq!(
            app.state
                .injuries
                .report(&(Season(2025), Week(1), SeasonType::Reg))
                .expect("pre-kickoff slate keeps its injury target")
                .by_gsis
                .get(&crate::entries::NflPlayerId("00-A".into()))
                .copied(),
            Some(InjuryDesignation::Questionable)
        );
        assert_eq!(
            provider.state.injury_calls.load(Ordering::SeqCst),
            1,
            "pre-kickoff reconcile fetches injuries once"
        );
        assert!(coordinator.feeds.is_empty());
        assert!(app.state.live.snapshot(contest).is_none());
        app.state.live.shutdown();
    }

    #[sqlx::test]
    async fn coordinator_skips_expired_slate_for_injuries(pool: SqlitePool) {
        let game = factories::game_at("2025_01_BUF_KC", 1, KICKOFF);
        let (app, contest) = app_with_slate(
            &pool,
            KICKOFF + time::Duration::hours(13),
            std::slice::from_ref(&game),
            "Expired Injuries",
        )
        .await;
        seed_alpha_runner(&app).await;
        let provider = FakeProvider::for_game(&game).with_injuries(alpha_injury_report());
        let mut coordinator = CoordinatorState::default();

        reconcile_once(&app.state, &provider, &mut coordinator)
            .await
            .unwrap();

        assert_eq!(
            provider.state.injury_calls.load(Ordering::SeqCst),
            0,
            "fully expired slate never fetches injuries"
        );
        assert!(
            app.state
                .injuries
                .report(&(Season(2025), Week(1), SeasonType::Reg))
                .is_none(),
            "fully expired slates never carry an injury target"
        );
        assert!(coordinator.feeds.is_empty());
        assert!(app.state.live.snapshot(contest).is_none());
        app.state.live.shutdown();
    }

    #[sqlx::test]
    async fn coordinator_shares_one_feed_for_same_slate(pool: SqlitePool) {
        let game = factories::game_at("2025_01_BUF_KC", 1, KICKOFF);
        let app = TestApp::from_pool_at(pool.clone(), KICKOFF + time::Duration::hours(1)).await;
        let contest_ids = [
            factories::contest_with_games(&pool, "Shared A", &[&game.gsis_game_id]).await,
            factories::contest_with_games(&pool, "Shared B", &[&game.gsis_game_id]).await,
        ];
        app.nfl
            .seed_for_test(&[], std::slice::from_ref(&game))
            .await
            .unwrap();
        let provider = FakeProvider::for_game(&game);
        let mut coordinator = CoordinatorState::default();

        reconcile_once(&app.state, &provider, &mut coordinator)
            .await
            .unwrap();

        assert_eq!(coordinator.feeds.len(), 1);
        let subscribers = coordinator
            .feeds
            .values()
            .next()
            .expect("shared feed")
            .state
            .lock()
            .expect("feed mutex poisoned")
            .contests
            .clone();
        assert_eq!(
            subscribers,
            contest_ids.into_iter().map(ContestId).collect()
        );
        for (_, runner) in coordinator.feeds.drain() {
            runner.handle.abort();
        }
    }
    #[sqlx::test]
    async fn coordinator_reaps_an_orphaned_feed(pool: SqlitePool) {
        let game = factories::game_at("2025_01_BUF_KC", 1, KICKOFF);
        let (app, contest) = app_with_slate(
            &pool,
            KICKOFF + time::Duration::hours(1),
            std::slice::from_ref(&game),
            "Orphaned",
        )
        .await;
        let provider = FakeProvider::for_game(&game);
        let mut coordinator = CoordinatorState::default();

        reconcile_once(&app.state, &provider, &mut coordinator)
            .await
            .unwrap();
        assert_eq!(coordinator.feeds.len(), 1);

        sqlx::query("DELETE FROM contest_games WHERE contest_id = ?1")
            .bind(contest.0)
            .execute(&pool)
            .await
            .unwrap();
        reconcile_once(&app.state, &provider, &mut coordinator)
            .await
            .unwrap();

        assert!(coordinator.feeds.is_empty());
        assert!(coordinator.replays.is_empty());
        app.state.live.shutdown();
    }
    #[sqlx::test]
    async fn coordinator_keeps_a_failed_feed_stopped(pool: SqlitePool) {
        let game = factories::game_at("2025_01_BUF_KC", 1, KICKOFF);
        let (app, _) = app_with_slate(
            &pool,
            KICKOFF + time::Duration::hours(1),
            std::slice::from_ref(&game),
            "Failed",
        )
        .await;
        let provider = FakeProvider::for_game(&game);
        let mut coordinator = CoordinatorState::default();

        reconcile_once(&app.state, &provider, &mut coordinator)
            .await
            .unwrap();
        let key = coordinator.feeds.keys().next().cloned().expect("feed");
        coordinator
            .feeds
            .get(&key)
            .expect("feed")
            .state
            .lock()
            .expect("feed mutex poisoned")
            .replay
            .terminal = FeedTerminal::Failed;
        coordinator.feeds.get(&key).expect("feed").handle.abort();
        tokio::task::yield_now().await;

        reconcile_once(&app.state, &provider, &mut coordinator)
            .await
            .unwrap();
        assert!(coordinator.feeds.is_empty());
        assert_eq!(
            coordinator
                .replays
                .get(&key)
                .expect("failed replay")
                .terminal,
            FeedTerminal::Failed
        );
        let scoreboard_calls = provider.state.scoreboard_calls.load(Ordering::SeqCst);

        reconcile_once(&app.state, &provider, &mut coordinator)
            .await
            .unwrap();
        assert!(coordinator.feeds.is_empty());
        assert_eq!(
            provider.state.scoreboard_calls.load(Ordering::SeqCst),
            scoreboard_calls
        );
        app.state.live.shutdown();
    }

    #[sqlx::test]
    async fn coordinator_reaps_completed_feed_into_replay_cache(pool: SqlitePool) {
        let game = factories::game_at("2025_01_BUF_KC", 1, KICKOFF);
        let (app, contest) = app_with_slate(
            &pool,
            KICKOFF + time::Duration::hours(1),
            std::slice::from_ref(&game),
            "Completed",
        )
        .await;
        let provider = FakeProvider::for_game(&game);
        let mut coordinator = CoordinatorState::default();

        reconcile_once(&app.state, &provider, &mut coordinator)
            .await
            .unwrap();
        let key = coordinator.feeds.keys().next().cloned().expect("feed");
        {
            let runner = coordinator.feeds.get(&key).expect("feed");
            wait_for(
                &provider.state,
                &provider.state.box_score_calls,
                &runner.handle,
            )
            .await;
        }
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let has_box_event = coordinator
                    .feeds
                    .get(&key)
                    .map(|runner| {
                        runner
                            .state
                            .lock()
                            .expect("feed mutex poisoned")
                            .replay
                            .events()
                            .iter()
                            .any(|event| {
                                matches!(event.as_ref(), tank01_data::PollEvent::BoxScore { .. })
                            })
                    })
                    .unwrap_or(false);
                if has_box_event {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("box event reaches replay");
        assert_eq!(
            app.state
                .live
                .snapshot(contest)
                .expect("live snapshot")
                .game_states
                .get(&game.gsis_game_id)
                .expect("game snapshot")
                .phase,
            nfl_data::LiveGamePhase::InProgress
        );
        coordinator
            .feeds
            .get(&key)
            .expect("feed")
            .state
            .lock()
            .expect("feed mutex poisoned")
            .replay
            .terminal = FeedTerminal::Completed;
        coordinator.feeds.get(&key).expect("feed").handle.abort();
        tokio::task::yield_now().await;
        coordinator.identity.clear();

        reconcile_once(&app.state, &provider, &mut coordinator)
            .await
            .unwrap();

        assert!(coordinator.feeds.is_empty());
        assert_eq!(
            coordinator
                .replays
                .get(&key)
                .expect("completed replay")
                .terminal,
            FeedTerminal::Completed
        );
        assert_eq!(
            app.state
                .live
                .snapshot(contest)
                .expect("rebased live snapshot")
                .game_states
                .get(&game.gsis_game_id)
                .expect("rebased game snapshot")
                .phase,
            nfl_data::LiveGamePhase::InProgress
        );
        app.state.live.shutdown();
    }

    #[sqlx::test]
    async fn coordinator_skips_slate_past_startup_recovery(pool: SqlitePool) {
        let game = factories::game_at("2025_01_BUF_KC", 1, KICKOFF);
        let (app, contest) = app_with_slate(
            &pool,
            KICKOFF + time::Duration::hours(13),
            std::slice::from_ref(&game),
            "Expired",
        )
        .await;
        let provider = FakeProvider::for_game(&game);
        let mut coordinator = CoordinatorState::default();

        reconcile_once(&app.state, &provider, &mut coordinator)
            .await
            .unwrap();

        assert!(coordinator.feeds.is_empty());
        assert!(app.state.live.snapshot(contest).is_none());
        app.state.live.shutdown();
    }

    #[sqlx::test]
    async fn coordinator_skips_mixed_week_slate(pool: SqlitePool) {
        let first = factories::game_at("2025_01_BUF_KC", 1, KICKOFF);
        let second = factories::game_at("2025_02_BUF_KC", 2, KICKOFF + time::Duration::days(7));
        let games = [first, second];
        let (app, contest) =
            app_with_slate(&pool, KICKOFF + time::Duration::hours(1), &games, "Mixed").await;
        let provider = FakeProvider::for_game(&games[0]);
        let mut coordinator = CoordinatorState::default();

        reconcile_once(&app.state, &provider, &mut coordinator)
            .await
            .unwrap();

        assert!(coordinator.feeds.is_empty());
        assert!(app.state.live.snapshot(contest).is_none());
        app.state.live.shutdown();
    }

    #[sqlx::test]
    async fn coordinator_excludes_official_slate(pool: SqlitePool) {
        let game = factories::game_at("2025_01_BUF_KC", 1, KICKOFF);
        let (app, contest) = app_with_slate(
            &pool,
            KICKOFF + time::Duration::hours(1),
            std::slice::from_ref(&game),
            "Official",
        )
        .await;
        let mut kc_player = factories::player_stats("00-KC", 1);
        kc_player.team = TeamAbbr("KC".into());
        kc_player.opponent = Some(TeamAbbr("BUF".into()));
        let mut buf_player = factories::player_stats("00-BUF", 1);
        buf_player.team = TeamAbbr("BUF".into());
        buf_player.opponent = Some(TeamAbbr("KC".into()));
        let mut defenses = [
            factories::defense_stats("KC", "BUF", 1),
            factories::defense_stats("BUF", "KC", 1),
        ];
        for defense in &mut defenses {
            defense.gsis_game_id = game.gsis_game_id.clone();
        }
        app.nfl
            .seed_week_stats_for_test(Season(2025), &[kc_player, buf_player], &defenses)
            .await
            .unwrap();
        let scores = load_contest_scores(&app.state, contest).await.unwrap();
        assert!(
            matches!(scores, ContestScores::Official(_)),
            "unexpected scores: {scores:?}"
        );
        let provider = FakeProvider::for_game(&game);
        let mut coordinator = CoordinatorState::default();

        reconcile_once(&app.state, &provider, &mut coordinator)
            .await
            .unwrap();

        assert!(coordinator.feeds.is_empty());
        assert!(app.state.live.snapshot(contest).is_none());
        app.state.live.shutdown();
    }

    #[test]
    fn feed_keys_include_sorted_provider_game_sets() {
        let one = FeedKey {
            date: time::macros::date!(2026 - 09 - 10),
            provider_game_ids: vec!["a".into(), "b".into()],
        };
        let two = FeedKey {
            date: time::macros::date!(2026 - 09 - 10),
            provider_game_ids: vec!["a".into(), "b".into()],
        };
        assert_eq!(one, two);
        assert_eq!(
            provider_game_id(&factories::game_at("2025_01_BUF_KC", 1, KICKOFF)),
            Some("20250907_BUF@KC".into())
        );
        let mut washington =
            factories::game_at("2026_01_WAS_PHI", 1, datetime!(2026 - 09 - 13 20:25 UTC));
        washington.home_team = TeamAbbr("PHI".into());
        washington.away_team = TeamAbbr("WAS".into());
        assert_eq!(
            provider_game_id(&washington),
            Some("20260913_WSH@PHI".into())
        );
    }

    #[test]
    fn injury_ttl_sharpens_around_the_slate_window() {
        let sharp = ::time::Duration::minutes(5);
        let idle = ::time::Duration::minutes(30);
        let now = datetime!(2025-09-07 17:00 UTC);
        let at = |offset: ::time::Duration| now + offset;
        let window =
            |first: OffsetDateTime, last: OffsetDateTime| Some(super::InjuryWindow { first, last });

        // Single-game window: sharp from 4h before kickoff.
        let single = window(now, now).expect("single-game window");
        assert_eq!(
            injury_ttl(at(::time::Duration::hours(-3)), Some(&single)),
            sharp,
            "3h before kickoff is sharp"
        );
        assert_eq!(
            injury_ttl(
                at(-::time::Duration::hours(4)) + ::time::Duration::seconds(1),
                Some(&single)
            ),
            sharp,
            "boundary one second inside the margin is sharp"
        );
        assert_eq!(
            injury_ttl(at(::time::Duration::hours(-5)), Some(&single)),
            idle,
            "5h before kickoff is idle"
        );
        assert_eq!(
            injury_ttl(
                at(::time::Duration::hours(3) + ::time::Duration::minutes(59)),
                Some(&single)
            ),
            sharp,
            "3h59 after kickoff is still sharp"
        );
        assert_eq!(
            injury_ttl(
                at(::time::Duration::hours(4) + ::time::Duration::seconds(1)),
                Some(&single)
            ),
            idle,
            "4h+1s after kickoff is idle"
        );

        // Two-game window: the tail boundary keys off the LAST kickoff.
        let two_game = window(
            at(::time::Duration::hours(-2)),
            at(::time::Duration::hours(9)),
        )
        .expect("two-game window");
        assert_eq!(
            injury_ttl(at(::time::Duration::hours(-6)), Some(&two_game)),
            sharp,
            "exact 4h margin from the earlier game"
        );
        assert_eq!(
            injury_ttl(
                at(::time::Duration::hours(9)) + ::time::Duration::minutes(3 * 60 + 59),
                Some(&two_game)
            ),
            sharp,
            "3h59 after the last kickoff is still sharp"
        );
        assert_eq!(
            injury_ttl(
                at(::time::Duration::hours(9)
                    + ::time::Duration::hours(4)
                    + ::time::Duration::seconds(1)),
                Some(&two_game)
            ),
            idle,
            "4h+1s after the last kickoff is idle"
        );

        assert_eq!(injury_ttl(now, None), idle, "no kickoffs means idle");
    }

    #[sqlx::test]
    async fn refresh_injuries_publishes_and_pauses_within_ttl(pool: SqlitePool) {
        let game = factories::game_at("2025_01_BUF_KC", 1, KICKOFF);
        let (app, _contest) = app_with_slate(
            &pool,
            KICKOFF + time::Duration::hours(1),
            std::slice::from_ref(&game),
            "Injured",
        )
        .await;
        seed_alpha_runner(&app).await;

        let provider = FakeProvider::for_game(&game).with_injuries(alpha_injury_report());
        let mut coordinator = CoordinatorState::default();

        reconcile_once(&app.state, &provider, &mut coordinator)
            .await
            .unwrap();
        let injuries = &app.state.injuries;
        let key = &(Season(2025), Week(1), SeasonType::Reg);
        let published = injuries.report(key).expect("injury report published");

        assert_eq!(
            published
                .by_gsis
                .get(&crate::entries::NflPlayerId("00-A".into()))
                .copied(),
            Some(InjuryDesignation::Questionable)
        );
        assert!(injuries.report(key).unwrap().attempted_at.is_some());
        assert_eq!(
            provider.state.injury_calls.load(Ordering::SeqCst),
            1,
            "one fetch serves the whole reconcile"
        );

        reconcile_once(&app.state, &provider, &mut coordinator)
            .await
            .unwrap();
        assert_eq!(
            provider.state.injury_calls.load(Ordering::SeqCst),
            1,
            "second reconcile within ttl does not refetch"
        );
        let published_again = injuries.report(key).unwrap();
        assert_eq!(published, published_again);
        app.state.live.shutdown();
    }
}
