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

use crate::live::LiveContestHub;
use crate::live::identity::LiveIdentityIndex;
use crate::live::snapshot::live_game;
use crate::{AppError, AppState};
use nfl_data::slates::SlateId;

const RECONCILE_INTERVAL: Duration = Duration::from_secs(20);
const STARTUP_RECOVERY: ::time::Duration = ::time::Duration::hours(12);

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct FeedKey {
    pub(crate) date: Date,
    pub(crate) tank01_game_ids: Vec<String>,
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
    contests: BTreeSet<SlateId>,
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
            candidate.game_id == game.game_id && candidate.tank01_game_id == game.tank01_game_id
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
                let replay = self.boxes.entry(game.game_id.clone()).or_default();
                replace_latest(&mut replay.latest_response, &event);
                if let ProviderOutcome::Value(snapshot) = &response.outcome {
                    replace_latest(&mut replay.latest_success, &event);
                    if snapshot.players.is_some() {
                        replace_latest(&mut replay.latest_player_section, &event);
                    }
                    if let Some(defenses) = &snapshot.defenses {
                        if defenses.iter().any(|defense| {
                            defense.team == game.home_team
                                && defense.dst_present
                                && defense.team_stats_present
                        }) {
                            replace_latest(&mut replay.latest_home_defense, &event);
                        }
                        if defenses.iter().any(|defense| {
                            defense.team == game.away_team
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
    contests: HashSet<SlateId>,
}

struct FeedRunner {
    state: Arc<Mutex<FeedState>>,
    handle: JoinHandle<()>,
}

#[derive(Default)]
struct CoordinatorState {
    feeds: HashMap<FeedKey, FeedRunner>,
    replays: HashMap<FeedKey, FeedReplay>,
    started: HashSet<SlateId>,
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
    let snapshot = state.nfl.snapshot();
    let revision = state.nfl.data_revision().await?;
    if coordinator.revision.as_ref() != Some(&revision) {
        coordinator.identity.clear();
        coordinator.revision = Some(revision);
    }

    let mut discovered = BTreeMap::<FeedKey, (LiveSlate, BTreeSet<SlateId>)>::new();
    let mut contest_updates =
        BTreeMap::<SlateId, (Vec<Game>, Arc<LiveIdentityIndex>, Vec<FeedKey>)>::new();
    let mut targets = HashMap::<crate::injuries::SlateKey, InjuryTarget>::new();

    for slate in snapshot.slates() {
        let games = snapshot.games_for_slate(&slate.id);
        if games.is_empty() {
            continue;
        }
        let first = games.iter().filter_map(Game::kickoff_eastern).min();
        let last = games.iter().filter_map(Game::kickoff_eastern).max();
        let started = coordinator.started.contains(&slate.id)
            || coordinator.feeds.values().any(|runner| {
                runner
                    .state
                    .lock()
                    .expect("feed mutex poisoned")
                    .contests
                    .contains(&slate.id)
            });

        let first_week = &games[0].season_week;
        let tuple = (first_week.season, first_week.week, first_week.season_type);
        if !games.iter().all(|game| {
            let sw = &game.season_week;
            (sw.season, sw.week, sw.season_type) == tuple
        }) {
            continue;
        }

        let mut teams = games
            .iter()
            .flat_map(|game| [game.home_team, game.away_team])
            .collect::<Vec<_>>();
        teams.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        teams.dedup_by(|a, b| a == b);

        if !started
            && first
                .zip(last)
                .is_some_and(|(first, last)| first > now || last < now - STARTUP_RECOVERY)
        {
            if last.is_some_and(|last| last < now - STARTUP_RECOVERY) {
                continue;
            }
            capture_injury_target(&mut targets, games, &tuple, &teams, slate.id);
            continue;
        }

        if matches!(
            crate::live::load_contest_scores(state, slate.id).await?,
            crate::live::ContestScores::Official(_)
        ) {
            state.live.notify(slate.id);
            continue;
        }

        capture_injury_target(&mut targets, games, &tuple, &teams, slate.id);

        let identity_key = (
            tuple.0,
            tuple.1,
            tuple.2,
            teams
                .iter()
                .map(|team| team.as_str().to_owned())
                .collect::<Vec<_>>(),
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
        for game in games {
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
                .map(|game| game.tank01_game_id.clone())
                .collect::<Vec<_>>();
            provider_ids.sort();
            provider_ids.dedup();
            let feed_key = FeedKey {
                date,
                tank01_game_ids: provider_ids,
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
                .insert(slate.id);
            feed_keys.push(feed_key);
        }
        feed_keys.sort();
        contest_updates.insert(slate.id, (games.to_vec(), identity, feed_keys));
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

    for slate in snapshot.slates() {
        if !snapshot.games_for_slate(&slate.id).is_empty() {
            state.live.notify(slate.id);
        }
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
    contest: SlateId,
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
            "injury refresh: nothing stale (no slate games, all fresh, or off-window)"
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
                        .map(|team| team.as_str().to_owned())
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

fn notify_contests(state: &AppState, contests: &BTreeSet<SlateId>) {
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

#[cfg(test)]
mod tests {
    use nfl_data::TeamAbbr;
    use time::OffsetDateTime;
    use time::macros::datetime;

    use super::{FeedKey, injury_ttl};
    use crate::live::tank01_game_id;
    use crate::tests::factories;

    const KICKOFF: OffsetDateTime = datetime!(2025-09-07 17:00 UTC);

    #[test]
    fn feed_keys_include_sorted_provider_game_sets() {
        let one = FeedKey {
            date: time::macros::date!(2026 - 09 - 10),
            tank01_game_ids: vec!["a".into(), "b".into()],
        };
        let two = FeedKey {
            date: time::macros::date!(2026 - 09 - 10),
            tank01_game_ids: vec!["a".into(), "b".into()],
        };
        assert_eq!(one, two);
        assert_eq!(
            tank01_game_id(&factories::game_at("2025_01_BUF_KC", 1, KICKOFF)),
            Some("20250907_BUF@KC".into())
        );
        let mut washington =
            factories::game_at("2026_01_WAS_PHI", 1, datetime!(2026 - 09 - 13 20:25 UTC));
        washington.home_team = TeamAbbr::PHI;
        washington.away_team = TeamAbbr::WAS;
        assert_eq!(tank01_game_id(&washington), Some("20260913_WSH@PHI".into()));
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
}
