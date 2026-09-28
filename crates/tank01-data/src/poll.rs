use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::Duration,
};

use ::time::Date;
use nfl_data::{
    LiveGame, LiveGamePhase, LiveGameSnapshot, LiveScoreProvider, LiveScoreboard, LiveSlate,
    ProviderOutcome, ProviderResponse,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::{
    sync::{Mutex, mpsc},
    task::JoinSet,
    time::{self, MissedTickBehavior},
};

use crate::normalize;

pub(crate) const POLL_INTERVAL_SECONDS: u64 = 20;
pub(crate) const FINAL_FOLLOWUP_INTERVAL_SECONDS: u64 = 300;
pub(crate) const PLAY_BY_PLAY_INTERVAL: u64 = 5;
pub(crate) const EVERY_FIFTH_PLAY_BY_PLAY: bool = PLAY_BY_PLAY_INTERVAL == 5;
const POLL_INTERVAL: Duration = Duration::from_secs(POLL_INTERVAL_SECONDS);
const FINAL_FOLLOWUP_INTERVAL: Duration = Duration::from_secs(FINAL_FOLLOWUP_INTERVAL_SECONDS);
pub(crate) const FINAL_FOLLOWUPS: u32 = 3;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PollRequest {
    pub endpoint: String,
    pub query: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompletionReason {
    FinalFollowupsComplete,
}

#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum PollEvent {
    Scoreboard {
        sequence: u64,
        request: PollRequest,
        response: ProviderResponse<LiveScoreboard>,
    },
    BoxScore {
        sequence: u64,
        game: LiveGame,
        request: PollRequest,
        play_by_play: bool,
        changed: bool,
        response: ProviderResponse<LiveGameSnapshot>,
    },
    RunFinished {
        sequence: u64,
        reason: CompletionReason,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum PollerError {
    #[error("invalid live slate: {0}")]
    InvalidSlate(String),
    #[error("poll event sink is closed")]
    SinkClosed,
    #[error("poll task failed to join: {0}")]
    TaskJoin(String),
}

#[derive(Clone)]
struct EventSink {
    sender: mpsc::Sender<PollEvent>,
    next_sequence: Arc<Mutex<u64>>,
}

impl EventSink {
    fn new(sender: mpsc::Sender<PollEvent>) -> Self {
        Self {
            sender,
            next_sequence: Arc::new(Mutex::new(1)),
        }
    }

    fn is_closed(&self) -> bool {
        self.sender.is_closed()
    }

    async fn closed(&self) {
        self.sender.closed().await;
    }

    async fn send(&self, event: PollEvent) -> Result<(), PollerError> {
        if self.sender.is_closed() {
            return Err(PollerError::SinkClosed);
        }
        let mut next_sequence = self.next_sequence.lock().await;
        if self.sender.is_closed() {
            return Err(PollerError::SinkClosed);
        }
        let sequence = *next_sequence;
        *next_sequence += 1;
        self.sender
            .send(with_sequence(event, sequence))
            .await
            .map_err(|_| PollerError::SinkClosed)
    }
}

fn with_sequence(event: PollEvent, sequence: u64) -> PollEvent {
    match event {
        PollEvent::Scoreboard {
            request, response, ..
        } => PollEvent::Scoreboard {
            sequence,
            request,
            response,
        },
        PollEvent::BoxScore {
            game,
            request,
            play_by_play,
            changed,
            response,
            ..
        } => PollEvent::BoxScore {
            sequence,
            game,
            request,
            play_by_play,
            changed,
            response,
        },
        PollEvent::RunFinished { reason, .. } => PollEvent::RunFinished { sequence, reason },
    }
}

pub struct SlatePoller<P> {
    provider: Arc<P>,
    slate: LiveSlate,
    sink: EventSink,
}

impl<P> SlatePoller<P>
where
    P: LiveScoreProvider + 'static,
{
    pub fn new(
        provider: P,
        slate: LiveSlate,
        sender: mpsc::Sender<PollEvent>,
    ) -> Result<Self, PollerError> {
        validate_slate(&slate)?;
        Ok(Self {
            provider: Arc::new(provider),
            slate,
            sink: EventSink::new(sender),
        })
    }

    pub async fn run(self) -> Result<(), PollerError> {
        let Self {
            provider,
            slate,
            sink,
        } = self;
        if sink.is_closed() {
            return Err(PollerError::SinkClosed);
        }
        let mut runners = JoinSet::new();
        let mut update_senders = Vec::with_capacity(slate.games.len());
        for game in slate.games.iter().cloned() {
            let (updates, receiver) = mpsc::unbounded_channel();
            update_senders.push(updates);
            let provider = Arc::clone(&provider);
            let sink = sink.clone();
            runners.spawn(run_game(provider, game, receiver, sink));
        }

        let mut scoreboard_interval = time::interval(POLL_INTERVAL);
        scoreboard_interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let _ = scoreboard_interval.tick().await;

        let initial = scoreboard_request(&provider, &sink, slate.date).await?;
        emit_scoreboard(&sink, slate.date, initial, &slate.games, &update_senders).await?;

        loop {
            if runners.is_empty() {
                sink.send(PollEvent::RunFinished {
                    sequence: 0,
                    reason: CompletionReason::FinalFollowupsComplete,
                })
                .await?;
                return Ok(());
            }
            tokio::select! {
                biased;
                joined = runners.join_next() => {
                    match joined {
                        Some(Ok(Ok(()))) => {}
                        Some(Ok(Err(error))) => {
                            return Err(error);
                        }
                        Some(Err(error)) => {
                            return Err(PollerError::TaskJoin(error.to_string()));
                        }
                        None => unreachable!("empty runner set handled above"),
                    }
                }
                _ = scoreboard_interval.tick() => {
                    if sink.is_closed() {
                        return Err(PollerError::SinkClosed);
                    }
                    let response = scoreboard_request(&provider, &sink, slate.date).await?;
                    emit_scoreboard(
                        &sink,
                        slate.date,
                        response,
                        &slate.games,
                        &update_senders,
                    )
                    .await?;
                }
            }
        }
    }
}

async fn scoreboard_request<P>(
    provider: &Arc<P>,
    sink: &EventSink,
    date: Date,
) -> Result<ProviderResponse<LiveScoreboard>, PollerError>
where
    P: LiveScoreProvider + 'static,
{
    if sink.is_closed() {
        return Err(PollerError::SinkClosed);
    }
    tokio::select! {
        _ = sink.closed() => Err(PollerError::SinkClosed),
        response = provider.scoreboard(date) => Ok(response),
    }
}

async fn box_score_request<P>(
    provider: &Arc<P>,
    sink: &EventSink,
    game: &LiveGame,
    play_by_play: bool,
) -> Result<ProviderResponse<LiveGameSnapshot>, PollerError>
where
    P: LiveScoreProvider + 'static,
{
    if sink.is_closed() {
        return Err(PollerError::SinkClosed);
    }
    tokio::select! {
        _ = sink.closed() => Err(PollerError::SinkClosed),
        response = provider.box_score(game, play_by_play) => Ok(response),
    }
}

async fn emit_scoreboard(
    sink: &EventSink,
    date: Date,
    response: ProviderResponse<LiveScoreboard>,
    games: &[LiveGame],
    update_senders: &[mpsc::UnboundedSender<ScoreboardUpdate>],
) -> Result<(), PollerError> {
    let request = PollRequest {
        endpoint: "getNFLScoresOnly".into(),
        query: BTreeMap::from([(String::from("gameDate"), normalize::compact_date(date))]),
    };
    let updates = scoreboard_updates(&response, games);
    sink.send(PollEvent::Scoreboard {
        sequence: 0,
        request,
        response,
    })
    .await?;
    for (index, update) in updates {
        let _ = update_senders[index].send(update);
    }
    Ok(())
}
pub(crate) fn box_score_query(game: &LiveGame, play_by_play: bool) -> BTreeMap<String, String> {
    let mut query = BTreeMap::from([(String::from("gameID"), game.provider_game_id.clone())]);
    if play_by_play {
        query.insert(String::from("playByPlay"), String::from("true"));
    }
    query
}

fn scoreboard_updates(
    response: &ProviderResponse<LiveScoreboard>,
    games: &[LiveGame],
) -> Vec<(usize, ScoreboardUpdate)> {
    let ProviderOutcome::Value(scoreboard) = &response.outcome else {
        return Vec::new();
    };
    let mut by_provider_id = BTreeMap::new();
    for game in &scoreboard.games {
        by_provider_id.insert(game.provider_game_id.as_str(), game.phase);
    }
    games
        .iter()
        .enumerate()
        .filter_map(|(index, game)| {
            by_provider_id
                .get(game.provider_game_id.as_str())
                .copied()
                .map(|phase| (index, ScoreboardUpdate { phase }))
        })
        .collect()
}

#[derive(Debug, Clone, Copy)]
struct ScoreboardUpdate {
    phase: LiveGamePhase,
}

async fn run_game<P>(
    provider: Arc<P>,
    game: LiveGame,
    mut updates: mpsc::UnboundedReceiver<ScoreboardUpdate>,
    sink: EventSink,
) -> Result<(), PollerError>
where
    P: LiveScoreProvider + 'static,
{
    let mut attempts = 0u64;
    let mut last_projection: Option<Value> = None;
    let mut active = false;
    let mut poll_interval: Option<time::Interval> = None;

    loop {
        if sink.is_closed() {
            return Err(PollerError::SinkClosed);
        }
        if let Some(interval) = &mut poll_interval {
            tokio::select! {
                update = updates.recv() => {
                    match update {
                        Some(update) if update.phase == LiveGamePhase::InProgress => {
                            active = true;
                        }
                        Some(update) if update.phase == LiveGamePhase::Final => {
                            request_box(
                                &provider,
                                &game,
                                &sink,
                                &mut attempts,
                                &mut last_projection,
                                false,
                            )
                            .await?;
                            run_final_followups(
                                &provider,
                                &game,
                                &sink,
                                &mut attempts,
                                &mut last_projection,
                            )
                            .await?;
                            return Ok(());
                        }
                        Some(_) => {}
                        None => return Err(PollerError::SinkClosed),
                    }
                }
                _ = interval.tick(), if active => {
                    let result = request_box(
                        &provider,
                        &game,
                        &sink,
                        &mut attempts,
                        &mut last_projection,
                        false,
                    )
                    .await?;
                    if result.successful_final {
                        run_final_followups(
                            &provider,
                            &game,
                            &sink,
                            &mut attempts,
                            &mut last_projection,
                        )
                        .await?;
                        return Ok(());
                    }
                }
            }
        } else {
            match updates.recv().await {
                Some(update) if update.phase == LiveGamePhase::InProgress => {
                    active = true;
                    let mut interval =
                        time::interval_at(time::Instant::now() + POLL_INTERVAL, POLL_INTERVAL);
                    interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
                    let result = request_box(
                        &provider,
                        &game,
                        &sink,
                        &mut attempts,
                        &mut last_projection,
                        false,
                    )
                    .await?;
                    if result.successful_final {
                        run_final_followups(
                            &provider,
                            &game,
                            &sink,
                            &mut attempts,
                            &mut last_projection,
                        )
                        .await?;
                        return Ok(());
                    }
                    poll_interval = Some(interval);
                }
                Some(update) if update.phase == LiveGamePhase::Final => {
                    request_box(
                        &provider,
                        &game,
                        &sink,
                        &mut attempts,
                        &mut last_projection,
                        false,
                    )
                    .await?;
                    run_final_followups(
                        &provider,
                        &game,
                        &sink,
                        &mut attempts,
                        &mut last_projection,
                    )
                    .await?;
                    return Ok(());
                }
                Some(_) => {}
                None => return Err(PollerError::SinkClosed),
            }
        }
    }
}

struct BoxRequestResult {
    successful_final: bool,
}

async fn request_box<P>(
    provider: &Arc<P>,
    game: &LiveGame,
    sink: &EventSink,
    attempts: &mut u64,
    last_projection: &mut Option<Value>,
    forced_play_by_play: bool,
) -> Result<BoxRequestResult, PollerError>
where
    P: LiveScoreProvider + 'static,
{
    if sink.is_closed() {
        return Err(PollerError::SinkClosed);
    }
    *attempts += 1;
    let play_by_play = forced_play_by_play || (*attempts).is_multiple_of(PLAY_BY_PLAY_INTERVAL);
    let response = box_score_request(provider, sink, game, play_by_play).await?;
    let successful_final = matches!(
        &response.outcome,
        ProviderOutcome::Value(snapshot) if snapshot.phase == LiveGamePhase::Final
    );
    let (changed, next_projection) = match &response.outcome {
        ProviderOutcome::Value(snapshot) => {
            let projection = normalize::response_projection(&response, snapshot);
            let changed = last_projection.as_ref() != Some(&projection);
            (changed, Some(projection))
        }
        ProviderOutcome::Pregame { .. } | ProviderOutcome::Error(_) => (false, None),
    };
    let request = PollRequest {
        endpoint: "getNFLBoxScore".into(),
        query: box_score_query(game, play_by_play),
    };
    sink.send(PollEvent::BoxScore {
        sequence: 0,
        game: game.clone(),
        request,
        play_by_play,
        changed,
        response,
    })
    .await?;
    if let Some(projection) = next_projection {
        *last_projection = Some(projection);
    }
    Ok(BoxRequestResult { successful_final })
}

async fn run_final_followups<P>(
    provider: &Arc<P>,
    game: &LiveGame,
    sink: &EventSink,
    attempts: &mut u64,
    last_projection: &mut Option<Value>,
) -> Result<(), PollerError>
where
    P: LiveScoreProvider + 'static,
{
    for _ in 0..FINAL_FOLLOWUPS {
        wait_or_sink_closed(sink, FINAL_FOLLOWUP_INTERVAL).await?;
        request_box(provider, game, sink, attempts, last_projection, true).await?;
    }
    Ok(())
}

async fn wait_or_sink_closed(sink: &EventSink, duration: Duration) -> Result<(), PollerError> {
    tokio::select! {
        _ = time::sleep(duration) => Ok(()),
        _ = sink.closed() => Err(PollerError::SinkClosed),
    }
}

fn validate_slate(slate: &LiveSlate) -> Result<(), PollerError> {
    if slate.games.is_empty() {
        return Err(PollerError::InvalidSlate("slate has no games".into()));
    }
    let mut gsis_ids = BTreeSet::new();
    let mut provider_ids = BTreeSet::new();
    let mut safe_names = BTreeMap::new();
    for game in &slate.games {
        if game.gsis_game_id.is_empty() {
            return Err(PollerError::InvalidSlate(
                "game has an empty GSIS game id".into(),
            ));
        }
        if game.provider_game_id.is_empty() {
            return Err(PollerError::InvalidSlate(
                "game has an empty provider game id".into(),
            ));
        }
        if !gsis_ids.insert(&game.gsis_game_id) {
            return Err(PollerError::InvalidSlate(format!(
                "duplicate GSIS game id {:?}",
                game.gsis_game_id
            )));
        }
        if !provider_ids.insert(&game.provider_game_id) {
            return Err(PollerError::InvalidSlate(format!(
                "duplicate provider game id {:?}",
                game.provider_game_id
            )));
        }
        let safe_name = safe_provider_game_id(&game.provider_game_id)?;
        if let Some(previous) = safe_names.insert(safe_name.clone(), game.provider_game_id.clone())
        {
            return Err(PollerError::InvalidSlate(format!(
                "provider game ids {:?} and {:?} collide as {safe_name:?}",
                previous, game.provider_game_id
            )));
        }
    }
    Ok(())
}

pub(crate) fn safe_provider_game_id(provider_game_id: &str) -> Result<String, PollerError> {
    let safe = provider_game_id.replace('@', "_at_");
    if safe.is_empty()
        || !safe
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
    {
        return Err(PollerError::InvalidSlate(format!(
            "provider game id {provider_game_id:?} cannot be used as a safe file name"
        )));
    }
    Ok(safe)
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use super::*;
    use ::time::{
        OffsetDateTime,
        macros::{date, datetime},
    };
    use nfl_data::{
        EspnPlayerId, LiveGamePhase, LiveGameSnapshot, LivePlayerStats, LiveProviderError,
        LiveScoreboardGame, LiveTeamStats, RawBody, Season, SeasonType, TeamAbbr, Week,
    };
    use serde_json::json;

    fn game(id: &str, provider_id: &str, home: &str, away: &str) -> LiveGame {
        LiveGame {
            gsis_game_id: id.into(),
            provider_game_id: provider_id.into(),
            season: Season(2026),
            week: Week(1),
            season_type: SeasonType::Reg,
            kickoff: Some(datetime!(2026-09-10 20:35:00 -04:00)),
            home_team: TeamAbbr(home.into()),
            away_team: TeamAbbr(away.into()),
        }
    }

    fn response<T>(outcome: ProviderOutcome<T>, raw_body: Option<RawBody>) -> ProviderResponse<T> {
        let now = OffsetDateTime::now_utc();
        ProviderResponse {
            requested_at: now,
            received_at: now,
            elapsed_ms: 1,
            http_status: Some(200),
            headers: BTreeMap::new(),
            raw_body,
            outcome,
        }
    }

    #[test]
    fn validates_nonempty_unique_and_safe_game_ids() {
        let (sender, _receiver) = mpsc::channel(1);
        let provider = FakeProvider::default();
        let slate = LiveSlate {
            date: date!(2026 - 09 - 10),
            games: vec![game("", "A", "LAR", "SF")],
        };
        assert!(matches!(
            SlatePoller::new(provider, slate, sender),
            Err(PollerError::InvalidSlate(_))
        ));
        assert_eq!(
            safe_provider_game_id("20260910_SF@LAR").unwrap(),
            "20260910_SF_at_LAR"
        );
        assert!(safe_provider_game_id("bad/path").is_err());
    }

    #[derive(Default)]
    struct FakeProvider {
        calls: AtomicUsize,
    }

    impl LiveScoreProvider for FakeProvider {
        async fn scoreboard(&self, _date: Date) -> ProviderResponse<LiveScoreboard> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            response(
                ProviderOutcome::Value(LiveScoreboard { games: vec![] }),
                Some(RawBody::Json(json!({"games": []}))),
            )
        }

        async fn box_score(
            &self,
            game: &LiveGame,
            _play_by_play: bool,
        ) -> ProviderResponse<LiveGameSnapshot> {
            response(
                ProviderOutcome::Value(LiveGameSnapshot {
                    game: game.clone(),
                    observed_at: OffsetDateTime::now_utc(),
                    phase: LiveGamePhase::InProgress,
                    period: Some("1".into()),
                    clock: Some("10:00".into()),
                    home_score: Some(0),
                    away_score: Some(0),
                    players: Some(vec![LivePlayerStats {
                        espn_id: Some(EspnPlayerId("1".into())),
                        name: Some("Player".into()),
                        team: Some(game.home_team.clone()),
                        position: Some("QB".into()),
                        opponent: Some(game.away_team.clone()),
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
                    }]),
                    defenses: Some(vec![LiveTeamStats {
                        team: game.home_team.clone(),
                        opponent: game.away_team.clone(),
                        sacks: 0,
                        interceptions: 0,
                        fumble_recoveries: 0,
                        safeties: 0,
                        touchdowns: 0,
                        blocked_kicks: 0,
                        conversion_returns: 0,
                        points_allowed: 0,
                        dst_present: true,
                        team_stats_present: true,
                    }]),
                }),
                Some(RawBody::Json(
                    json!({"playerStats": {"1": {"rushingYards": 0}}}),
                )),
            )
        }
    }
    #[derive(Debug, Clone)]
    struct RecordedCall {
        provider_game_id: String,
        attempt: u64,
        play_by_play: bool,
    }

    #[derive(Clone, Default)]
    struct LifecycleState {
        calls: Arc<Mutex<Vec<RecordedCall>>>,
        in_flight: Arc<AtomicUsize>,
        max_in_flight: Arc<AtomicUsize>,
        scoreboards: Arc<AtomicUsize>,
    }

    impl LifecycleState {
        async fn record(&self, provider_game_id: &str, play_by_play: bool) -> u64 {
            let mut calls = self.calls.lock().await;
            let attempt = calls
                .iter()
                .filter(|call| call.provider_game_id == provider_game_id)
                .count() as u64
                + 1;
            calls.push(RecordedCall {
                provider_game_id: provider_game_id.into(),
                attempt,
                play_by_play,
            });
            attempt
        }

        fn enter(&self) {
            let current = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_in_flight.fetch_max(current, Ordering::SeqCst);
        }

        fn leave(&self) {
            self.in_flight.fetch_sub(1, Ordering::SeqCst);
        }
    }

    #[derive(Clone, Default)]
    struct LifecycleProvider {
        state: LifecycleState,
    }

    impl LiveScoreProvider for LifecycleProvider {
        async fn scoreboard(&self, _date: Date) -> ProviderResponse<LiveScoreboard> {
            let scoreboard_number = self.state.scoreboards.fetch_add(1, Ordering::SeqCst) + 1;
            self.state.enter();
            tokio::task::yield_now().await;
            self.state.leave();
            response(
                ProviderOutcome::Value(LiveScoreboard {
                    games: vec![
                        LiveScoreboardGame {
                            provider_game_id: "20260910_SF@LAR".into(),
                            phase: LiveGamePhase::InProgress,
                            period: Some("1".into()),
                            clock: Some("10:00".into()),
                            home_team: TeamAbbr("LAR".into()),
                            away_team: TeamAbbr("SF".into()),
                            home_score: Some(0),
                            away_score: Some(0),
                        },
                        LiveScoreboardGame {
                            provider_game_id: "20260910_NE@NYJ".into(),
                            phase: LiveGamePhase::InProgress,
                            period: Some("1".into()),
                            clock: Some("10:00".into()),
                            home_team: TeamAbbr("NYJ".into()),
                            away_team: TeamAbbr("NE".into()),
                            home_score: Some(0),
                            away_score: Some(0),
                        },
                        LiveScoreboardGame {
                            provider_game_id: "20260910_DAL@NYG".into(),
                            phase: if scoreboard_number == 1 {
                                LiveGamePhase::Scheduled
                            } else {
                                LiveGamePhase::Final
                            },
                            period: None,
                            clock: None,
                            home_team: TeamAbbr("NYG".into()),
                            away_team: TeamAbbr("DAL".into()),
                            home_score: None,
                            away_score: None,
                        },
                    ],
                }),
                Some(RawBody::Json(json!({"games": []}))),
            )
        }

        async fn box_score(
            &self,
            game: &LiveGame,
            play_by_play: bool,
        ) -> ProviderResponse<LiveGameSnapshot> {
            let attempt = self
                .state
                .record(&game.provider_game_id, play_by_play)
                .await;
            self.state.enter();
            tokio::task::yield_now().await;
            self.state.leave();
            if attempt == 5 {
                return response(
                    ProviderOutcome::Error(LiveProviderError::Transport {
                        message: "simulated PBP failure".into(),
                    }),
                    None,
                );
            }
            let phase = if game.provider_game_id == "20260910_DAL@NYG" || attempt >= 7 {
                LiveGamePhase::Final
            } else {
                LiveGamePhase::InProgress
            };
            response(
                ProviderOutcome::Value(LiveGameSnapshot {
                    game: game.clone(),
                    observed_at: OffsetDateTime::now_utc(),
                    phase,
                    period: Some("1".into()),
                    clock: Some("10:00".into()),
                    home_score: Some(7),
                    away_score: Some(3),
                    players: None,
                    defenses: None,
                }),
                Some(RawBody::Json(json!({
                    "gameID": game.provider_game_id,
                    "gameStatusCode": if phase == LiveGamePhase::Final { "3" } else { "1" },
                    "attempt": attempt,
                }))),
            )
        }
    }

    #[tokio::test]
    async fn stops_before_provider_requests_when_sink_is_closed() {
        let provider = FakeProvider::default();
        let slate = LiveSlate {
            date: date!(2026 - 09 - 10),
            games: vec![game("A", "20260910_SF@LAR", "LAR", "SF")],
        };
        let (sender, receiver) = mpsc::channel(1);
        drop(receiver);

        let poller = SlatePoller::new(provider, slate, sender).unwrap();
        assert!(matches!(poller.run().await, Err(PollerError::SinkClosed)));
    }

    #[tokio::test(start_paused = true)]
    async fn runs_active_games_concurrently_through_final_followups() {
        let provider = LifecycleProvider::default();
        let state = provider.state.clone();
        let slate = LiveSlate {
            date: date!(2026 - 09 - 10),
            games: vec![
                game("A", "20260910_SF@LAR", "LAR", "SF"),
                game("B", "20260910_NE@NYJ", "NYJ", "NE"),
                game("C", "20260910_DAL@NYG", "NYG", "DAL"),
            ],
        };
        let (sender, mut receiver) = mpsc::channel(512);
        let poller = SlatePoller::new(provider, slate, sender).unwrap();
        let mut run = Box::pin(poller.run());
        let mut run_result = None;
        for _ in 0..100 {
            tokio::select! {
                result = &mut run => {
                    run_result = Some(result);
                    break;
                }
                _ = tokio::task::yield_now() => {}
            }
            time::advance(POLL_INTERVAL).await;
        }
        if run_result.is_none() {
            for _ in 0..10 {
                tokio::select! {
                    result = &mut run => {
                        run_result = Some(result);
                        break;
                    }
                    _ = tokio::task::yield_now() => {}
                }
            }
        }
        assert!(
            run_result.is_some(),
            "poller did not finish in virtual time: {:?}",
            state.calls.lock().await
        );
        assert!(matches!(run_result.unwrap(), Ok(())));

        let mut events = Vec::new();
        while let Some(event) = receiver.recv().await {
            events.push(event);
        }

        assert!(matches!(
            events.first(),
            Some(PollEvent::Scoreboard { sequence: 1, .. })
        ));
        assert!(matches!(
            events.last(),
            Some(PollEvent::RunFinished {
                reason: CompletionReason::FinalFollowupsComplete,
                ..
            })
        ));
        for (index, event) in events.iter().enumerate() {
            let sequence = match event {
                PollEvent::Scoreboard { sequence, .. }
                | PollEvent::BoxScore { sequence, .. }
                | PollEvent::RunFinished { sequence, .. } => *sequence,
            };
            assert_eq!(sequence, index as u64 + 1);
        }

        let calls = state.calls.lock().await.clone();
        assert!(state.max_in_flight.load(Ordering::SeqCst) >= 2);
        let scoreboard_phases = events
            .iter()
            .filter_map(|event| match event {
                PollEvent::Scoreboard { response, .. } => match &response.outcome {
                    ProviderOutcome::Value(scoreboard) => scoreboard
                        .games
                        .iter()
                        .find(|game| game.provider_game_id == "20260910_DAL@NYG")
                        .map(|game| game.phase),
                    _ => None,
                },
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(scoreboard_phases[0], LiveGamePhase::Scheduled);
        assert_eq!(scoreboard_phases[1], LiveGamePhase::Final);
        let second_scoreboard_position = events
            .iter()
            .enumerate()
            .filter(|(_, event)| matches!(event, PollEvent::Scoreboard { .. }))
            .nth(1)
            .map(|(index, _)| index)
            .unwrap();
        let scheduled_game_box_position = events
            .iter()
            .position(|event| {
                matches!(
                    event,
                    PollEvent::BoxScore { game, .. }
                        if game.provider_game_id == "20260910_DAL@NYG"
                )
            })
            .unwrap();
        assert!(scheduled_game_box_position > second_scoreboard_position);
        let scheduled_game_calls = calls
            .iter()
            .filter(|call| call.provider_game_id == "20260910_DAL@NYG")
            .collect::<Vec<_>>();
        assert_eq!(scheduled_game_calls.len(), 4);
        assert_eq!(
            scheduled_game_calls
                .iter()
                .map(|call| call.play_by_play)
                .collect::<Vec<_>>(),
            vec![false, true, true, true]
        );
        for provider_game_id in ["20260910_SF@LAR", "20260910_NE@NYJ"] {
            let game_calls = calls
                .iter()
                .filter(|call| call.provider_game_id == provider_game_id)
                .collect::<Vec<_>>();
            assert_eq!(game_calls.len(), 10);
            assert_eq!(
                game_calls
                    .iter()
                    .filter(|call| call.play_by_play)
                    .map(|call| call.attempt)
                    .collect::<Vec<_>>(),
                vec![5, 8, 9, 10]
            );
        }
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(
                    event,
                    PollEvent::BoxScore { game, .. }
                        if game.provider_game_id == "20260910_SF@LAR"
                ))
                .count(),
            10
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(
                    event,
                    PollEvent::BoxScore { game, .. }
                        if game.provider_game_id == "20260910_NE@NYJ"
                ))
                .count(),
            10
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(
                    event,
                    PollEvent::BoxScore {
                        response: ProviderResponse {
                            outcome: ProviderOutcome::Error(LiveProviderError::Transport { .. }),
                            ..
                        },
                        play_by_play: true,
                        ..
                    }
                ))
                .count(),
            2
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(
                    event,
                    PollEvent::BoxScore {
                        play_by_play: false,
                        response: ProviderResponse {
                            outcome: ProviderOutcome::Value(snapshot),
                            ..
                        },
                        ..
                    } if snapshot.phase == LiveGamePhase::Final
                ))
                .count(),
            3
        );
    }
}
