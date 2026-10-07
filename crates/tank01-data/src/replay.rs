use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};

use nfl_model::{
    LiveGameSnapshot, LiveProviderError, LiveScoreboard, ProviderOutcome, ProviderResponse, RawBody,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    capture::{
        CaptureError, LineEnvelope, Manifest, SCHEMA_VERSION, game_file_name, read_manifest,
    },
    normalize,
    poll::{
        EVERY_FIFTH_PLAY_BY_PLAY, FINAL_FOLLOWUP_INTERVAL_SECONDS, FINAL_FOLLOWUPS,
        POLL_INTERVAL_SECONDS, PollEvent, PollRequest, box_score_query, safe_provider_game_id,
    },
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayMismatch {
    pub sequence: Option<u64>,
    pub file: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayReport {
    pub valid: bool,
    pub completed: bool,
    pub event_count: u64,
    pub mismatches: Vec<ReplayMismatch>,
}

#[derive(Debug, thiserror::Error)]
pub enum ReplayError {
    #[error("replay I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("replay JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid replay capture: {0}")]
    Invalid(String),
}

pub struct Replay;

impl Replay {
    pub fn validate(path: impl AsRef<Path>) -> Result<ReplayReport, ReplayError> {
        let root = path.as_ref();
        let manifest = read_manifest(root).map_err(capture_error)?;
        let mut mismatches = Vec::new();
        validate_manifest(root, &manifest, &mut mismatches)?;

        let mut streams = Vec::new();
        let scoreboard_path = root.join("scoreboard.ndjson");
        streams.push(read_stream(
            &scoreboard_path,
            "scoreboard.ndjson",
            &mut mismatches,
        )?);
        for game in &manifest.games {
            if expected_game_file(&game.provider_game_id).as_deref()
                != Some(game.file_name.as_str())
            {
                continue;
            }
            streams.push(read_stream(
                &root.join(&game.file_name),
                &game.file_name,
                &mut mismatches,
            )?);
        }

        for stream in &streams {
            validate_stream_order(stream, &mut mismatches);
        }
        validate_global_order(&streams, &manifest, &mut mismatches);

        let mut ordered = streams
            .into_iter()
            .flat_map(|stream| stream.events)
            .collect::<Vec<_>>();
        ordered.sort_by_key(|event| event.sequence());
        let game_by_provider = manifest
            .games
            .iter()
            .map(|game| (game.provider_game_id.as_str(), game))
            .collect::<BTreeMap<_, _>>();
        let mut previous_projection: BTreeMap<String, Value> = BTreeMap::new();

        for stored in &ordered {
            match stored.event() {
                PollEvent::Scoreboard {
                    request, response, ..
                } => {
                    validate_request(
                        stored,
                        request,
                        "getNFLScoresOnly",
                        BTreeMap::from([(
                            String::from("gameDate"),
                            normalize::compact_date(manifest.slate_date),
                        )]),
                        &mut mismatches,
                    );
                    replay_scoreboard(stored, response, &mut mismatches);
                }
                PollEvent::BoxScore {
                    game,
                    request,
                    response,
                    play_by_play,
                    changed,
                    ..
                } => {
                    validate_request(
                        stored,
                        request,
                        "getNFLBoxScore",
                        box_score_query(game, *play_by_play),
                        &mut mismatches,
                    );
                    match game_by_provider.get(game.provider_game_id.as_str()) {
                        Some(manifest_game) => {
                            if manifest_game.gsis_game_id != game.gsis_game_id {
                                mismatch(
                                    &mut mismatches,
                                    Some(stored.sequence()),
                                    &stored.file,
                                    format!(
                                        "box event GSIS id {:?} disagrees with manifest {:?}",
                                        game.gsis_game_id, manifest_game.gsis_game_id
                                    ),
                                );
                            }
                            if stored.file != manifest_game.file_name {
                                mismatch(
                                    &mut mismatches,
                                    Some(stored.sequence()),
                                    &stored.file,
                                    format!(
                                        "box event is in {:?}, expected {:?}",
                                        stored.file, manifest_game.file_name
                                    ),
                                );
                            }
                        }
                        None => mismatch(
                            &mut mismatches,
                            Some(stored.sequence()),
                            &stored.file,
                            format!(
                                "box event references provider game {:?} absent from manifest",
                                game.provider_game_id
                            ),
                        ),
                    }
                    replay_box(
                        stored,
                        game,
                        response,
                        *changed,
                        &mut previous_projection,
                        &mut mismatches,
                    );
                }
                PollEvent::RunFinished { .. } => mismatch(
                    &mut mismatches,
                    Some(stored.sequence()),
                    &stored.file,
                    "terminal event must not appear in a request stream".into(),
                ),
            }
        }

        Ok(ReplayReport {
            valid: mismatches.is_empty(),
            completed: manifest.completed_at.is_some()
                && manifest.completion_reason.is_some()
                && manifest.terminal_sequence.is_some(),
            event_count: ordered.len() as u64,
            mismatches,
        })
    }
}

struct StoredStream {
    file: String,
    events: Vec<StoredEvent>,
}

struct StoredEvent {
    file: String,
    envelope: LineEnvelope,
}

impl StoredEvent {
    fn sequence(&self) -> u64 {
        event_sequence(&self.envelope.event)
    }

    fn event(&self) -> &PollEvent {
        &self.envelope.event
    }
}

fn read_stream(
    path: &Path,
    file: &str,
    mismatches: &mut Vec<ReplayMismatch>,
) -> Result<StoredStream, ReplayError> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(StoredStream {
                file: file.into(),
                events: Vec::new(),
            });
        }
        Err(error) => return Err(error.into()),
    };
    let mut events = Vec::new();
    for (line_index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let envelope = match serde_json::from_str::<LineEnvelope>(line) {
            Ok(envelope) => envelope,
            Err(error) => {
                mismatch(
                    mismatches,
                    None,
                    file,
                    format!(
                        "line {} is not a valid versioned event: {error}",
                        line_index + 1
                    ),
                );
                continue;
            }
        };
        if envelope.schema_version != SCHEMA_VERSION {
            mismatch(
                mismatches,
                Some(event_sequence(&envelope.event)),
                file,
                format!(
                    "line {} has schema version {}, expected {}",
                    line_index + 1,
                    envelope.schema_version,
                    SCHEMA_VERSION
                ),
            );
        }
        events.push(StoredEvent {
            file: file.into(),
            envelope,
        });
    }
    Ok(StoredStream {
        file: file.into(),
        events,
    })
}

fn validate_manifest(
    root: &Path,
    manifest: &Manifest,
    mismatches: &mut Vec<ReplayMismatch>,
) -> Result<(), ReplayError> {
    if manifest.schema_version != SCHEMA_VERSION {
        return Err(ReplayError::Invalid(format!(
            "manifest schema version {} is unsupported",
            manifest.schema_version
        )));
    }
    if manifest.games.is_empty() {
        return Err(ReplayError::Invalid("manifest has no games".into()));
    }
    if manifest.policy.scoreboard_cadence_seconds != POLL_INTERVAL_SECONDS
        || manifest.policy.box_score_cadence_seconds != POLL_INTERVAL_SECONDS
        || manifest.policy.every_fifth_play_by_play != EVERY_FIFTH_PLAY_BY_PLAY
        || manifest.policy.final_followups != FINAL_FOLLOWUPS
        || manifest.policy.final_followup_interval_seconds != FINAL_FOLLOWUP_INTERVAL_SECONDS
    {
        mismatch(
            mismatches,
            None,
            "manifest.json",
            "manifest polling policy does not match schema version 1".into(),
        );
    }
    let mut gsis_ids = BTreeSet::new();
    let mut provider_ids = BTreeSet::new();
    let mut file_names = BTreeSet::new();
    for game in &manifest.games {
        if game.gsis_game_id.is_empty() || game.provider_game_id.is_empty() {
            mismatch(
                mismatches,
                None,
                "manifest.json",
                "manifest contains an empty game identity".into(),
            );
        }
        if !gsis_ids.insert(&game.gsis_game_id) {
            mismatch(
                mismatches,
                None,
                "manifest.json",
                format!("duplicate GSIS game id {:?}", game.gsis_game_id),
            );
        }
        if !provider_ids.insert(&game.provider_game_id) {
            mismatch(
                mismatches,
                None,
                "manifest.json",
                format!("duplicate provider game id {:?}", game.provider_game_id),
            );
        }
        if !file_names.insert(&game.file_name) {
            mismatch(
                mismatches,
                None,
                "manifest.json",
                format!("duplicate game file {:?}", game.file_name),
            );
        }
        let expected_file = expected_game_file(&game.provider_game_id);
        if expected_file.as_deref() != Some(game.file_name.as_str()) {
            mismatch(
                mismatches,
                None,
                "manifest.json",
                format!(
                    "game file {:?} does not match provider game id {:?}",
                    game.file_name, game.provider_game_id
                ),
            );
        } else if !root.join(&game.file_name).exists() {
            mismatch(
                mismatches,
                None,
                "manifest.json",
                format!("missing game stream {:?}", game.file_name),
            );
        }
    }
    if !root.join("scoreboard.ndjson").exists() {
        mismatch(
            mismatches,
            None,
            "manifest.json",
            "missing scoreboard stream".into(),
        );
    }
    let completed_at = manifest.completed_at.is_some();
    let completion_reason = manifest.completion_reason.is_some();
    let terminal_sequence = manifest.terminal_sequence.is_some();
    if completed_at != completion_reason || completed_at != terminal_sequence {
        mismatch(
            mismatches,
            manifest.terminal_sequence,
            "manifest.json",
            "completed_at, completion_reason, and terminal_sequence must be recorded together"
                .into(),
        );
    }
    Ok(())
}

fn validate_stream_order(stream: &StoredStream, mismatches: &mut Vec<ReplayMismatch>) {
    let mut previous = None;
    for event in &stream.events {
        let sequence = event.sequence();
        if previous.is_some_and(|previous| sequence <= previous) {
            mismatch(
                mismatches,
                Some(sequence),
                &stream.file,
                format!("stream sequence {sequence} is not greater than {previous:?}"),
            );
        }
        previous = Some(sequence);
        match (stream.file.as_str(), event.event()) {
            ("scoreboard.ndjson", PollEvent::Scoreboard { .. }) => {}
            ("scoreboard.ndjson", _) => mismatch(
                mismatches,
                Some(sequence),
                &stream.file,
                "non-scoreboard event appeared in scoreboard stream".into(),
            ),
            (_, PollEvent::BoxScore { .. }) => {}
            (_, _) => mismatch(
                mismatches,
                Some(sequence),
                &stream.file,
                "non-box event appeared in game stream".into(),
            ),
        }
    }
}

fn validate_global_order(
    streams: &[StoredStream],
    manifest: &Manifest,
    mismatches: &mut Vec<ReplayMismatch>,
) {
    let mut all = streams
        .iter()
        .flat_map(|stream| {
            stream
                .events
                .iter()
                .map(|event| (event.sequence(), &event.file))
        })
        .collect::<Vec<_>>();
    all.sort_by_key(|(sequence, _)| *sequence);
    let mut previous = None;
    let mut expected = 1u64;
    for (sequence, file) in &all {
        if previous == Some(*sequence) {
            mismatch(
                mismatches,
                Some(*sequence),
                file,
                "global event sequence is duplicated".into(),
            );
        }
        if *sequence != expected {
            mismatch(
                mismatches,
                Some(*sequence),
                file,
                format!("global event sequence is {sequence}, expected {expected}"),
            );
        }
        previous = Some(*sequence);
        expected = sequence.saturating_add(1);
    }
    if let Some(terminal_sequence) = manifest.terminal_sequence {
        if all
            .iter()
            .any(|(sequence, _)| *sequence == terminal_sequence)
        {
            mismatch(
                mismatches,
                Some(terminal_sequence),
                "manifest.json",
                "terminal sequence collides with a request event".into(),
            );
        }
        if all
            .iter()
            .any(|(sequence, _)| *sequence >= terminal_sequence)
        {
            mismatch(
                mismatches,
                Some(terminal_sequence),
                "manifest.json",
                "terminal sequence is not after every request event".into(),
            );
        }
        if terminal_sequence != expected {
            mismatch(
                mismatches,
                Some(terminal_sequence),
                "manifest.json",
                format!(
                    "terminal sequence {terminal_sequence} does not follow request sequence {}",
                    expected.saturating_sub(1)
                ),
            );
        }
    }
}

fn expected_game_file(provider_game_id: &str) -> Option<String> {
    safe_provider_game_id(provider_game_id)
        .ok()
        .map(|safe_name| game_file_name(&safe_name))
}

fn validate_request(
    stored: &StoredEvent,
    request: &PollRequest,
    endpoint: &str,
    query: BTreeMap<String, String>,
    mismatches: &mut Vec<ReplayMismatch>,
) {
    if request.endpoint != endpoint || request.query != query {
        mismatch(
            mismatches,
            Some(stored.sequence()),
            &stored.file,
            format!(
                "request metadata differs: captured {} {:?}, expected {endpoint} {:?}",
                request.endpoint, request.query, query
            ),
        );
    }
}

fn replay_scoreboard(
    stored: &StoredEvent,
    response: &ProviderResponse<LiveScoreboard>,
    mismatches: &mut Vec<ReplayMismatch>,
) {
    let replayed = classify_scoreboard(response);
    compare_outcomes(
        stored.sequence(),
        &stored.file,
        &response.outcome,
        &replayed,
        mismatches,
    );
}

fn replay_box(
    stored: &StoredEvent,
    game: &nfl_model::LiveGame,
    response: &ProviderResponse<LiveGameSnapshot>,
    changed: bool,
    previous_projection: &mut BTreeMap<String, Value>,
    mismatches: &mut Vec<ReplayMismatch>,
) {
    let replayed = classify_box(response, game);
    compare_outcomes(
        stored.sequence(),
        &stored.file,
        &response.outcome,
        &replayed,
        mismatches,
    );
    let expected_changed = match (&response.outcome, &replayed) {
        (ProviderOutcome::Value(_), ReplayOutcome::Value(replayed_snapshot)) => {
            let projection = normalize::response_projection(response, replayed_snapshot);
            let expected = !previous_projection.contains_key(&game.provider_game_id)
                || previous_projection.get(&game.provider_game_id) != Some(&projection);
            previous_projection.insert(game.provider_game_id.clone(), projection);
            expected
        }
        _ => false,
    };
    if changed != expected_changed {
        mismatch(
            mismatches,
            Some(stored.sequence()),
            &stored.file,
            format!("changed marker was {changed}, replay calculated {expected_changed}"),
        );
    }
}

enum ReplayOutcome<T> {
    Value(T),
    Pregame,
    Error(OutcomeCategory),
}

#[derive(Debug, PartialEq, Eq)]
enum OutcomeCategory {
    Transport,
    Http(Option<u16>),
    Json,
    Normalization,
    IdentityConflict,
}

fn classify_scoreboard(
    response: &ProviderResponse<LiveScoreboard>,
) -> ReplayOutcome<LiveScoreboard> {
    if !is_success_status(response.http_status) {
        return ReplayOutcome::Error(OutcomeCategory::Http(response.http_status));
    }
    let Some(RawBody::Json(value)) = &response.raw_body else {
        return match &response.outcome {
            ProviderOutcome::Error(LiveProviderError::Transport { .. }) => {
                ReplayOutcome::Error(OutcomeCategory::Transport)
            }
            ProviderOutcome::Error(LiveProviderError::Json { .. }) => {
                ReplayOutcome::Error(OutcomeCategory::Json)
            }
            _ => ReplayOutcome::Error(OutcomeCategory::Json),
        };
    };
    match normalize::normalize_scoreboard(value) {
        Ok(value) => ReplayOutcome::Value(value),
        Err(error) => ReplayOutcome::Error(error_category(error.into_provider_error())),
    }
}

fn classify_box(
    response: &ProviderResponse<LiveGameSnapshot>,
    game: &nfl_model::LiveGame,
) -> ReplayOutcome<LiveGameSnapshot> {
    if !is_success_status(response.http_status) {
        return ReplayOutcome::Error(OutcomeCategory::Http(response.http_status));
    }
    let Some(raw_body) = &response.raw_body else {
        return ReplayOutcome::Error(OutcomeCategory::Transport);
    };
    let RawBody::Json(value) = raw_body else {
        return ReplayOutcome::Error(OutcomeCategory::Json);
    };
    match normalize::pregame_message_for(value, Some(&game.provider_game_id)) {
        Ok(Some(_)) => return ReplayOutcome::Pregame,
        Ok(None) => {}
        Err(error) => return ReplayOutcome::Error(error_category(error.into_provider_error())),
    }
    match normalize::normalize_box_score(value, game, response.received_at) {
        Ok(value) => ReplayOutcome::Value(value),
        Err(error) => ReplayOutcome::Error(error_category(error.into_provider_error())),
    }
}

fn compare_outcomes<T: PartialEq>(
    sequence: u64,
    file: &str,
    recorded: &ProviderOutcome<T>,
    replayed: &ReplayOutcome<T>,
    mismatches: &mut Vec<ReplayMismatch>,
) {
    match (recorded, replayed) {
        (ProviderOutcome::Value(recorded), ReplayOutcome::Value(replayed)) => {
            if recorded != replayed {
                mismatch(
                    mismatches,
                    Some(sequence),
                    file,
                    "normalized successful response differs from captured response".into(),
                );
            }
        }
        (ProviderOutcome::Pregame { .. }, ReplayOutcome::Pregame) => {}
        (ProviderOutcome::Error(error), ReplayOutcome::Error(category))
            if error_category(error.clone()) == *category => {}
        (recorded, replayed) => mismatch(
            mismatches,
            Some(sequence),
            file,
            format!(
                "outcome differs: captured {}, replayed {}",
                outcome_label(recorded),
                replayed_label(replayed)
            ),
        ),
    }
}

fn is_success_status(status: Option<u16>) -> bool {
    status.is_none_or(|status| (200..300).contains(&status))
}

fn error_category(error: LiveProviderError) -> OutcomeCategory {
    match error {
        LiveProviderError::Transport { .. } => OutcomeCategory::Transport,
        LiveProviderError::Http { status, .. } => OutcomeCategory::Http(Some(status)),
        LiveProviderError::Json { .. } => OutcomeCategory::Json,
        LiveProviderError::Normalization { .. } => OutcomeCategory::Normalization,
        LiveProviderError::IdentityConflict { .. } => OutcomeCategory::IdentityConflict,
    }
}

fn outcome_label<T>(outcome: &ProviderOutcome<T>) -> &'static str {
    match outcome {
        ProviderOutcome::Value(_) => "value",
        ProviderOutcome::Pregame { .. } => "pregame",
        ProviderOutcome::Error(LiveProviderError::Transport { .. }) => "transport error",
        ProviderOutcome::Error(LiveProviderError::Http { .. }) => "HTTP error",
        ProviderOutcome::Error(LiveProviderError::Json { .. }) => "JSON error",
        ProviderOutcome::Error(LiveProviderError::Normalization { .. }) => "normalization error",
        ProviderOutcome::Error(LiveProviderError::IdentityConflict { .. }) => "identity conflict",
    }
}

fn replayed_label<T>(outcome: &ReplayOutcome<T>) -> &'static str {
    match outcome {
        ReplayOutcome::Value(_) => "value",
        ReplayOutcome::Pregame => "pregame",
        ReplayOutcome::Error(OutcomeCategory::Transport) => "transport error",
        ReplayOutcome::Error(OutcomeCategory::Http(_)) => "HTTP error",
        ReplayOutcome::Error(OutcomeCategory::Json) => "JSON error",
        ReplayOutcome::Error(OutcomeCategory::Normalization) => "normalization error",
        ReplayOutcome::Error(OutcomeCategory::IdentityConflict) => "identity conflict",
    }
}

fn event_sequence(event: &PollEvent) -> u64 {
    match event {
        PollEvent::Scoreboard { sequence, .. }
        | PollEvent::BoxScore { sequence, .. }
        | PollEvent::RunFinished { sequence, .. } => *sequence,
    }
}

fn mismatch(
    mismatches: &mut Vec<ReplayMismatch>,
    sequence: Option<u64>,
    file: &str,
    message: String,
) {
    mismatches.push(ReplayMismatch {
        sequence,
        file: file.into(),
        message,
    });
}

fn capture_error(error: CaptureError) -> ReplayError {
    match error {
        CaptureError::Io(error) => ReplayError::Io(error),
        CaptureError::Json(error) => ReplayError::Json(error),
        CaptureError::Invalid(message) => ReplayError::Invalid(message),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::{
        capture::CaptureWriter,
        poll::{CompletionReason, PollRequest},
    };
    use nfl_model::{
        LiveGame, LiveGamePhase, LiveScoreboard, LiveScoreboardGame, LiveSlate, ProviderOutcome,
        ProviderResponse, Season, SeasonType, TeamAbbr, Week,
    };
    use serde_json::json;
    use time::macros::{date, datetime};

    fn game() -> LiveGame {
        LiveGame {
            gsis_game_id: "2026_01_SF_LAR".into(),
            provider_game_id: "20260910_SF@LAR".into(),
            season: Season(2026),
            week: Week(1),
            season_type: SeasonType::Reg,
            kickoff: Some(datetime!(2026-09-10 20:35:00 -04:00)),
            home_team: TeamAbbr("LA".into()),
            away_team: TeamAbbr("SF".into()),
        }
    }

    fn scoreboard(sequence: u64) -> PollEvent {
        let now = datetime!(2026-09-11 01:00:00 UTC);
        PollEvent::Scoreboard {
            sequence,
            request: PollRequest {
                endpoint: "getNFLScoresOnly".into(),
                query: BTreeMap::from([(String::from("gameDate"), String::from("20260910"))]),
            },
            response: ProviderResponse {
                requested_at: now,
                received_at: now,
                elapsed_ms: 1,
                http_status: Some(200),
                headers: BTreeMap::new(),
                raw_body: Some(RawBody::Json(
                    json!({"20260910_SF@LAR":{"gameID":"20260910_SF@LAR","home":"LAR","away":"SF","gameStatusCode":"1"}}),
                )),
                outcome: ProviderOutcome::Value(LiveScoreboard {
                    games: vec![LiveScoreboardGame {
                        provider_game_id: "20260910_SF@LAR".into(),
                        phase: LiveGamePhase::InProgress,
                        period: None,
                        clock: None,
                        home_team: TeamAbbr("LA".into()),
                        away_team: TeamAbbr("SF".into()),
                        home_score: None,
                        away_score: None,
                    }],
                }),
            },
        }
    }

    fn box_event(sequence: u64, yards: i32) -> PollEvent {
        let game = game();
        let now = datetime!(2026-09-11 01:00:01 UTC);
        let raw = json!({"gameID":"20260910_SF@LAR","gameStatusCode":"1","playerStats":{"1":{"playerID":"1","team":"SF","Rushing":{"rushYds":yards}}}});
        let snapshot = normalize::normalize_box_score(&raw, &game, now).unwrap();
        PollEvent::BoxScore {
            sequence,
            game,
            request: PollRequest {
                endpoint: "getNFLBoxScore".into(),
                query: BTreeMap::from([(String::from("gameID"), String::from("20260910_SF@LAR"))]),
            },
            play_by_play: false,
            changed: sequence == 2,
            response: ProviderResponse {
                requested_at: now,
                received_at: now,
                elapsed_ms: 1,
                http_status: Some(200),
                headers: BTreeMap::new(),
                raw_body: Some(RawBody::Json(raw)),
                outcome: ProviderOutcome::Value(snapshot),
            },
        }
    }

    #[tokio::test]
    async fn validates_capture_round_trip_and_reports_mutated_raw_response() {
        let directory = tempfile::tempdir().unwrap();
        let slate = LiveSlate {
            date: date!(2026 - 09 - 10),
            games: vec![game()],
        };
        let writer = CaptureWriter::create(directory.path(), &slate).unwrap();
        let (sender, receiver) = tokio::sync::mpsc::channel(8);
        sender.send(scoreboard(1)).await.unwrap();
        sender.send(box_event(2, 4)).await.unwrap();
        sender
            .send(PollEvent::RunFinished {
                sequence: 3,
                reason: CompletionReason::FinalFollowupsComplete,
            })
            .await
            .unwrap();
        drop(sender);
        writer.consume(receiver).await.unwrap();
        let valid = Replay::validate(directory.path()).unwrap();
        assert!(valid.valid, "{valid:?}");
        let path = directory.path().join("games/20260910_SF_at_LAR.ndjson");
        let line = fs::read_to_string(&path).unwrap();
        let mut envelope: LineEnvelope = serde_json::from_str(line.trim()).unwrap();
        match &mut envelope.event {
            PollEvent::BoxScore {
                response:
                    ProviderResponse {
                        raw_body: Some(RawBody::Json(raw)),
                        ..
                    },
                ..
            } => {
                raw["playerStats"]["1"]["Rushing"]["rushYds"] = json!(5);
            }
            _ => panic!("expected a box-score event"),
        }
        fs::write(
            path,
            format!("{}\n", serde_json::to_string(&envelope).unwrap()),
        )
        .unwrap();
        let report = Replay::validate(directory.path()).unwrap();
        assert!(!report.valid);
        assert!(
            report
                .mismatches
                .iter()
                .any(|m| { m.sequence == Some(2) && m.file.contains("20260910_SF_at_LAR") })
        );
    }

    #[tokio::test]
    async fn reports_missing_game_stream_without_aborting_replay() {
        let directory = tempfile::tempdir().unwrap();
        let slate = LiveSlate {
            date: date!(2026 - 09 - 10),
            games: vec![game()],
        };
        let writer = CaptureWriter::create(directory.path(), &slate).unwrap();
        let (sender, receiver) = tokio::sync::mpsc::channel(8);
        sender.send(scoreboard(1)).await.unwrap();
        sender
            .send(PollEvent::RunFinished {
                sequence: 2,
                reason: CompletionReason::FinalFollowupsComplete,
            })
            .await
            .unwrap();
        drop(sender);
        writer.consume(receiver).await.unwrap();
        fs::remove_file(directory.path().join("games/20260910_SF_at_LAR.ndjson")).unwrap();

        let report = Replay::validate(directory.path()).unwrap();
        assert!(!report.valid);
        assert!(
            report.mismatches.iter().any(|m| {
                m.file == "manifest.json" && m.message.contains("missing game stream")
            })
        );
    }

    #[tokio::test]
    async fn reports_box_events_written_to_the_wrong_game_stream() {
        let directory = tempfile::tempdir().unwrap();
        let mut second_game = game();
        second_game.gsis_game_id = "2026_01_NE_NYJ".into();
        second_game.provider_game_id = "20260910_NE@NYJ".into();
        second_game.home_team = TeamAbbr("NYJ".into());
        second_game.away_team = TeamAbbr("NE".into());
        let slate = LiveSlate {
            date: date!(2026 - 09 - 10),
            games: vec![game(), second_game],
        };
        let writer = CaptureWriter::create(directory.path(), &slate).unwrap();
        let (sender, receiver) = tokio::sync::mpsc::channel(8);
        sender.send(scoreboard(1)).await.unwrap();
        sender.send(box_event(2, 4)).await.unwrap();
        sender
            .send(PollEvent::RunFinished {
                sequence: 3,
                reason: CompletionReason::FinalFollowupsComplete,
            })
            .await
            .unwrap();
        drop(sender);
        writer.consume(receiver).await.unwrap();

        let first = directory.path().join("games/20260910_SF_at_LAR.ndjson");
        let second = directory.path().join("games/20260910_NE_at_NYJ.ndjson");
        fs::remove_file(&second).unwrap();
        fs::rename(first, second).unwrap();

        let report = Replay::validate(directory.path()).unwrap();
        assert!(!report.valid);
        assert!(report.mismatches.iter().any(|m| {
            m.file == "games/20260910_NE_at_NYJ.ndjson" && m.message.contains("expected")
        }));
    }

    #[tokio::test]
    async fn reports_inconsistent_completion_metadata() {
        let directory = tempfile::tempdir().unwrap();
        let slate = LiveSlate {
            date: date!(2026 - 09 - 10),
            games: vec![game()],
        };
        let writer = CaptureWriter::create(directory.path(), &slate).unwrap();
        let (sender, receiver) = tokio::sync::mpsc::channel(8);
        sender
            .send(PollEvent::RunFinished {
                sequence: 1,
                reason: CompletionReason::FinalFollowupsComplete,
            })
            .await
            .unwrap();
        drop(sender);
        writer.consume(receiver).await.unwrap();

        let manifest_path = directory.path().join("manifest.json");
        let mut manifest: Value =
            serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
        manifest["completed_at"] = Value::Null;
        fs::write(manifest_path, serde_json::to_vec_pretty(&manifest).unwrap()).unwrap();

        let report = Replay::validate(directory.path()).unwrap();
        assert!(!report.valid);
        assert!(!report.completed);
        assert!(report.mismatches.iter().any(|m| {
            m.file == "manifest.json"
                && m.message
                    .contains("completed_at, completion_reason, and terminal_sequence")
        }));
    }
}
