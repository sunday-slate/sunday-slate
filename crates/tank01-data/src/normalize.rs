use std::collections::{BTreeMap, BTreeSet};

use nfl_data::{
    EspnPlayerId, LiveGame, LiveGamePhase, LiveGameSnapshot, LivePlayerStats, LiveProviderError,
    LiveScoreboard, LiveScoreboardGame, LiveTeamStats, ProviderResponse, RawBody, TeamAbbr,
};
use serde_json::{Map, Value};
use time::{Date, OffsetDateTime};

const PLAYER_ID_KEYS: &[&str] = &["playerID", "playerId", "espnID", "espnId", "espn_id", "id"];
const PLAYER_NAME_KEYS: &[&str] = &["longName", "fullName", "playerName", "name"];
const PLAYER_TEAM_KEYS: &[&str] = &["team", "teamAbv", "teamAbbr", "teamName", "abbreviation"];
const PLAYER_POSITION_KEYS: &[&str] = &["position", "pos", "positionGroup"];
const GAME_ID_KEYS: &[&str] = &[
    "gameID",
    "gameId",
    "providerGameId",
    "provider_game_id",
    "id",
];
const HOME_TEAM_KEYS: &[&str] = &["home", "homeTeam", "homeTeamAbv", "homeTeamAbbr"];
const AWAY_TEAM_KEYS: &[&str] = &["away", "awayTeam", "awayTeamAbv", "awayTeamAbbr"];
const HOME_SCORE_KEYS: &[&str] = &["homePts", "homeScore", "homePoints"];
const AWAY_SCORE_KEYS: &[&str] = &["awayPts", "awayScore", "awayPoints"];
const STATUS_CODE_KEYS: &[&str] = &[
    "gameStatusCode",
    "game_status_code",
    "statusCode",
    "status_code",
];
const STATUS_KEYS: &[&str] = &["gameStatus", "game_status", "status", "phase"];
const PERIOD_KEYS: &[&str] = &["currentPeriod", "current_period", "period", "quarter"];
const CLOCK_KEYS: &[&str] = &["gameClock", "game_clock", "clock", "time"];

const PASSING_YARDS: &[&str] = &[
    "passYards",
    "passingYards",
    "passingYds",
    "passYds",
    "yardsPassing",
];
const PASSING_TDS: &[&str] = &[
    "passTD",
    "passingTD",
    "passingTDs",
    "passTouchdowns",
    "passingTouchdowns",
];
const PASSING_INTERCEPTIONS: &[&str] = &[
    "int",
    "passInt",
    "passInts",
    "passingInt",
    "passingInts",
    "passingInterceptions",
];
const COMPLETIONS: &[&str] = &[
    "passCompletions",
    "passingCompletions",
    "completions",
    "complete",
];
const ATTEMPTS: &[&str] = &["passAttempts", "passingAttempts", "attempts"];
const RUSHING_ATTEMPTS: &[&str] = &[
    "carries",
    "rushAttempts",
    "rushingAttempts",
    "rushingCarries",
];
const RUSHING_YARDS: &[&str] = &[
    "rushYards",
    "rushingYards",
    "rushYds",
    "rushingYds",
    "yardsRushing",
];
const RUSHING_TDS: &[&str] = &[
    "rushTD",
    "rushTDs",
    "rushingTD",
    "rushingTDs",
    "rushTouchdowns",
    "rushingTouchdowns",
];
const TARGETS: &[&str] = &["targets", "target"];
const RECEPTIONS: &[&str] = &["receptions", "reception", "rec"];
const RECEIVING_YARDS: &[&str] = &[
    "recYards",
    "receivingYards",
    "recYds",
    "receivingYds",
    "yardsReceiving",
];
const RECEIVING_TDS: &[&str] = &[
    "recTD",
    "recTDs",
    "receivingTD",
    "receivingTDs",
    "recTouchdowns",
    "receivingTouchdowns",
];
const FUMBLES_LOST: &[&str] = &["fumblesLost", "lostFumbles", "fumbleLost"];
const PASSING_TWO_POINT: &[&str] = &[
    "passing2PT",
    "passing2PTs",
    "passing2Point",
    "passing2PointConversions",
    "pass2PT",
    "pass2PTs",
    "passTwo",
];
const RUSHING_TWO_POINT: &[&str] = &[
    "rushing2PT",
    "rushing2PTs",
    "rushing2Point",
    "rushing2PointConversions",
    "rush2PT",
    "rush2PTs",
    "rushTwo",
];
const RECEIVING_TWO_POINT: &[&str] = &[
    "receiving2PT",
    "receiving2PTs",
    "receiving2Point",
    "receiving2PointConversions",
    "rec2PT",
    "rec2PTs",
    "recTwo",
];
const TWO_POINT: &[&str] = &[
    "twoPointConversions",
    "twoPointConversion",
    "2PTConversions",
    "2PTConversion",
];
const SPECIAL_TEAMS_TDS: &[&str] = &[
    "specialTeamsTD",
    "specialTeamsTDs",
    "returnTD",
    "returnTDs",
    "kickReturnTD",
    "puntReturnTD",
];
const FUMBLE_RECOVERY_TDS: &[&str] = &[
    "fumbleRecoveryTD",
    "fumbleRecoveryTDs",
    "fumbleRecoveryTouchdowns",
    "fumbleReturnTD",
    "fumbleReturnTDs",
];

const SACKS: &[&str] = &["sacks", "sack", "defSacks", "defensiveSacks"];
const INTERCEPTIONS: &[&str] = &[
    "interceptions",
    "interception",
    "defInterceptions",
    "defInt",
    "defensiveInterceptions",
];
const FUMBLE_RECOVERIES: &[&str] = &[
    "fumbleRecoveries",
    "fumbleRecovery",
    "fumblesRecovered",
    "fumblesRecovery",
    "defFumbleRecoveries",
];
const SAFETIES: &[&str] = &["safeties", "safety", "defensiveSafeties"];
const DEFENSIVE_TDS: &[&str] = &[
    "touchdowns",
    "touchdown",
    "defTD",
    "defTDs",
    "defensiveTD",
    "defensiveTDs",
    "defensiveTouchdowns",
    "defensiveOrSpecialTeamsTDs",
];
const BLOCKED_KICKS: &[&str] = &[
    "blockedKicks",
    "blockedKick",
    "blockedFG",
    "blockedPunts",
    "blockedPunt",
    "blockedXP",
    "blockedFieldGoals",
    "blockedFieldGoal",
    "blockedExtraPoints",
    "blockedExtraPoint",
];
const CONVERSION_RETURNS: &[&str] = &[
    "conversionReturns",
    "conversionReturn",
    "twoPointReturns",
    "twoPointReturn",
    "defensiveTwoPointReturns",
    "defensiveTwoPointConversionReturns",
    "extraPointReturns",
    "extraPointReturn",
];
const POINTS_ALLOWED: &[&str] = &[
    "ptsAllowed",
    "pointsAllowed",
    "pointsAllowedCount",
    "defensivePointsAllowed",
];

const PLAYER_SECTION_KEYS: &[&str] = &[
    "passing",
    "rushing",
    "receiving",
    "kicking",
    "defense",
    "fumbles",
];
const PLAYER_STAT_KEYS: &[&[&str]] = &[
    PASSING_YARDS,
    PASSING_TDS,
    PASSING_INTERCEPTIONS,
    COMPLETIONS,
    ATTEMPTS,
    RUSHING_ATTEMPTS,
    RUSHING_YARDS,
    RUSHING_TDS,
    TARGETS,
    RECEPTIONS,
    RECEIVING_YARDS,
    RECEIVING_TDS,
    FUMBLES_LOST,
    PASSING_TWO_POINT,
    RUSHING_TWO_POINT,
    RECEIVING_TWO_POINT,
    TWO_POINT,
    SPECIAL_TEAMS_TDS,
    FUMBLE_RECOVERY_TDS,
];
const DST_STAT_KEYS: &[&[&str]] = &[
    SACKS,
    INTERCEPTIONS,
    FUMBLE_RECOVERIES,
    SAFETIES,
    DEFENSIVE_TDS,
    BLOCKED_KICKS,
    CONVERSION_RETURNS,
    POINTS_ALLOWED,
];

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum NormalizeError {
    #[error("normalization failed: {0}")]
    Invalid(String),
    #[error("identity conflict: {0}")]
    IdentityConflict(String),
}

impl NormalizeError {
    pub(crate) fn into_provider_error(self) -> LiveProviderError {
        match self {
            Self::Invalid(message) => LiveProviderError::Normalization { message },
            Self::IdentityConflict(message) => LiveProviderError::IdentityConflict { message },
        }
    }
}

pub(crate) fn normalize_scoreboard(value: &Value) -> Result<LiveScoreboard, NormalizeError> {
    reject_provider_errors(value)?;
    let body = endpoint_body(value);
    let records = scoreboard_records(body)?;

    records
        .into_iter()
        .map(|(hint, record)| normalize_scoreboard_game(record, hint.as_deref()))
        .collect::<Result<Vec<_>, _>>()
        .map(|games| LiveScoreboard { games })
}

pub(crate) fn normalize_box_score(
    value: &Value,
    game: &LiveGame,
    observed_at: OffsetDateTime,
) -> Result<LiveGameSnapshot, NormalizeError> {
    let body_value = box_body(value, Some(&game.provider_game_id))?;
    let body = body_value
        .as_object()
        .ok_or_else(|| NormalizeError::Invalid("box score body is not an object".into()))?;
    reject_unknown_error(body)?;
    let provider_game_id = direct_string(body, GAME_ID_KEYS)?;
    if let Some(provider_game_id) = provider_game_id.as_deref()
        && provider_game_id != game.provider_game_id
    {
        return Err(NormalizeError::Invalid(format!(
            "box response game id {provider_game_id:?} does not match requested {:?}",
            game.provider_game_id
        )));
    }

    let phase = game_phase(body);
    let period = optional_display(body, PERIOD_KEYS)?;
    let clock = optional_display(body, CLOCK_KEYS)?;
    let home_score = optional_score(body, HOME_SCORE_KEYS)?;
    let away_score = optional_score(body, AWAY_SCORE_KEYS)?;
    let players = normalize_players(body, game)?;
    let defenses = normalize_defenses(body, game)?;

    Ok(LiveGameSnapshot {
        game: game.clone(),
        observed_at,
        phase,
        period,
        clock,
        home_score,
        away_score,
        players,
        defenses,
    })
}

#[cfg(test)]
pub(crate) fn pregame_message(value: &Value) -> Option<String> {
    pregame_message_for(value, None).ok().flatten()
}

pub(crate) fn pregame_message_for(
    value: &Value,
    requested: Option<&str>,
) -> Result<Option<String>, NormalizeError> {
    let body = box_body(value, requested)?;
    let object = body
        .as_object()
        .ok_or_else(|| NormalizeError::Invalid("box score body is not an object".into()))?;
    let Some(message) = direct_scalar(object, &["error", "message"]) else {
        return Ok(None);
    };
    let message = display_value(message)?;
    let lower = message.to_ascii_lowercase();
    Ok((lower.contains("hasn't started")
        || lower.contains("has not started")
        || lower.contains("not started")
        || lower.contains("will start"))
    .then_some(message))
}

pub(crate) fn stable_projection(value: &Value) -> Value {
    let body = box_body(value, None).unwrap_or(value);
    let relevant = if let Value::Object(object) = body {
        let selected = object
            .iter()
            .filter(|(key, _)| is_stable_section(key))
            .map(|(key, value)| (key.clone(), canonicalize(value)))
            .collect::<BTreeMap<_, _>>();
        if selected.is_empty() {
            object
                .iter()
                .filter(|(key, _)| !is_volatile_metadata(key))
                .map(|(key, value)| (key.clone(), canonicalize(value)))
                .collect::<BTreeMap<_, _>>()
        } else {
            selected
        }
    } else {
        BTreeMap::from([(String::from("body"), canonicalize(body))])
    };
    Value::Object(relevant.into_iter().collect())
}
pub(crate) fn response_projection(
    response: &ProviderResponse<LiveGameSnapshot>,
    snapshot: &LiveGameSnapshot,
) -> Value {
    match &response.raw_body {
        Some(RawBody::Json(value)) => stable_projection(value),
        _ => serde_json::json!({"players": snapshot.players, "defenses": snapshot.defenses}),
    }
}

pub(crate) fn compact_date(date: Date) -> String {
    date.format(time::macros::format_description!(
        "[year][month padding:zero][day padding:zero]"
    ))
    .expect("compact date format is valid")
}

fn normalize_scoreboard_game(
    record: &Value,
    hint: Option<&str>,
) -> Result<LiveScoreboardGame, NormalizeError> {
    let object = record
        .as_object()
        .ok_or_else(|| NormalizeError::Invalid("scoreboard game is not an object".into()))?;
    reject_unknown_error(object)?;
    let explicit_provider_game_id = direct_string(object, GAME_ID_KEYS)?;
    if let (Some(hint), Some(explicit)) = (hint, explicit_provider_game_id.as_deref())
        && hint != explicit
    {
        return Err(NormalizeError::IdentityConflict(format!(
            "scoreboard map id {hint:?} disagrees with game id {explicit:?}"
        )));
    }
    let provider_game_id = explicit_provider_game_id
        .or_else(|| hint.map(str::to_owned))
        .ok_or_else(|| NormalizeError::Invalid("scoreboard game has no provider game id".into()))?;
    let home_team = scoreboard_team(object, HOME_TEAM_KEYS, "home")?;
    let away_team = scoreboard_team(object, AWAY_TEAM_KEYS, "away")?;
    if home_team == away_team {
        return Err(NormalizeError::Invalid(format!(
            "scoreboard game {provider_game_id:?} has the same home and away team"
        )));
    }
    Ok(LiveScoreboardGame {
        provider_game_id,
        phase: game_phase(object),
        period: optional_display(object, PERIOD_KEYS)?,
        clock: optional_display(object, CLOCK_KEYS)?,
        home_team,
        away_team,
        home_score: optional_score(object, HOME_SCORE_KEYS)?,
        away_score: optional_score(object, AWAY_SCORE_KEYS)?,
    })
}

fn scoreboard_records(body: &Value) -> Result<Vec<(Option<String>, &Value)>, NormalizeError> {
    if let Some(object) = body.as_object()
        && let Some(section) = direct_value(object, &["games", "scores", "events"])
    {
        return collection_records(section, "scoreboard");
    }
    match body {
        Value::Array(records) => Ok(records.iter().map(|record| (None, record)).collect()),
        Value::Object(object) => {
            if looks_like_scoreboard_record(object) {
                return Ok(vec![(None, body)]);
            }
            let records = object
                .iter()
                .filter(|(key, value)| {
                    !is_wrapper_metadata(key) && matches!(value, Value::Object(_) | Value::Array(_))
                })
                .map(|(key, value)| (Some(key.clone()), value))
                .collect::<Vec<_>>();
            Ok(records)
        }
        _ => Err(NormalizeError::Invalid(
            "scoreboard body is not an object or list".into(),
        )),
    }
}

fn collection_records<'a>(
    section: &'a Value,
    name: &str,
) -> Result<Vec<(Option<String>, &'a Value)>, NormalizeError> {
    match section {
        Value::Array(records) => Ok(records.iter().map(|record| (None, record)).collect()),
        Value::Object(object) => {
            if looks_like_scoreboard_record(object) {
                Ok(vec![(None, section)])
            } else {
                Ok(object
                    .iter()
                    .map(|(key, value)| (Some(key.clone()), value))
                    .collect())
            }
        }
        Value::Null => Ok(Vec::new()),
        _ => Err(NormalizeError::Invalid(format!(
            "{name} section is not a list or object"
        ))),
    }
}

fn looks_like_scoreboard_record(object: &Map<String, Value>) -> bool {
    has_any_key(object, GAME_ID_KEYS)
        || has_any_key(object, HOME_TEAM_KEYS)
        || has_any_key(object, AWAY_TEAM_KEYS)
        || has_any_key(object, STATUS_KEYS)
        || has_any_key(object, STATUS_CODE_KEYS)
}

fn endpoint_body(value: &Value) -> &Value {
    let mut current = value;
    for _ in 0..3 {
        let Some(object) = current.as_object() else {
            break;
        };
        let Some(next) = direct_value(object, &["body", "data"]) else {
            break;
        };
        if next.is_object() || next.is_array() {
            current = next;
        } else {
            break;
        }
    }
    current
}

fn box_body<'a>(value: &'a Value, requested: Option<&str>) -> Result<&'a Value, NormalizeError> {
    let mut body = endpoint_body(value);
    for _ in 0..3 {
        let Some(object) = body.as_object() else {
            break;
        };
        if let Some(game) = direct_value(object, &["game"])
            && game.is_object()
        {
            body = game;
            continue;
        }
        if let Some(requested) = requested
            && let Some(game) = object.get(requested)
            && game.is_object()
        {
            body = game;
            continue;
        }
        let candidates = object
            .iter()
            .filter(|(key, value)| {
                !is_wrapper_metadata(key)
                    && matches!(value, Value::Object(_))
                    && value.as_object().is_some_and(looks_like_box_record)
            })
            .collect::<Vec<_>>();
        if let Some(requested) = requested
            && !candidates.is_empty()
            && !candidates.iter().any(|(key, _)| key.as_str() == requested)
        {
            return Err(NormalizeError::IdentityConflict(format!(
                "box response map has no requested game id {requested:?}"
            )));
        }
        if candidates.len() == 1 {
            let (key, candidate) = candidates[0];
            if let Some(requested) = requested
                && key.as_str() != requested
            {
                return Err(NormalizeError::IdentityConflict(format!(
                    "box response map id {key:?} does not match requested {requested:?}"
                )));
            }
            body = candidate;
            continue;
        }
        break;
    }
    if body.is_object() {
        Ok(body)
    } else {
        Err(NormalizeError::Invalid(
            "box score body is not an object".into(),
        ))
    }
}

fn looks_like_box_record(object: &Map<String, Value>) -> bool {
    has_any_key(
        object,
        &[
            "playerStats",
            "DST",
            "teamStats",
            "scoringPlays",
            "allPlayByPlay",
            "error",
            "message",
        ],
    ) || has_any_key(object, GAME_ID_KEYS)
        || has_any_key(object, STATUS_KEYS)
        || has_any_key(object, STATUS_CODE_KEYS)
}
fn reject_provider_errors(value: &Value) -> Result<(), NormalizeError> {
    let mut current = value;
    for _ in 0..=3 {
        let Some(object) = current.as_object() else {
            break;
        };
        reject_unknown_error(object)?;
        let Some(next) = direct_value(object, &["body", "data"]) else {
            break;
        };
        if !next.is_object() && !next.is_array() {
            break;
        }
        current = next;
    }
    Ok(())
}

fn reject_unknown_error(body: &Map<String, Value>) -> Result<(), NormalizeError> {
    let Some(error) = direct_value(body, &["error"]) else {
        return Ok(());
    };
    if error.is_null() || is_empty_marker(error) {
        return Ok(());
    }
    let message = display_value(error).unwrap_or_else(|_| {
        serde_json::to_string(error).unwrap_or_else(|_| String::from("<unprintable>"))
    });
    Err(NormalizeError::Invalid(format!(
        "provider returned an unrecognized error: {message}"
    )))
}

fn game_phase(body: &Map<String, Value>) -> LiveGamePhase {
    if let Some(code) = direct_scalar(body, STATUS_CODE_KEYS)
        && let Some(code) = parse_status_code(code)
    {
        return match code {
            0 => LiveGamePhase::Scheduled,
            1 => LiveGamePhase::InProgress,
            2 => LiveGamePhase::Final,
            _ => LiveGamePhase::Unknown,
        };
    }
    let Some(status) = direct_scalar(body, STATUS_KEYS)
        .and_then(display_value_opt)
        .or_else(|| direct_scalar(body, PERIOD_KEYS).and_then(display_value_opt))
    else {
        return LiveGamePhase::Unknown;
    };
    let status = status.to_ascii_lowercase();
    if status.contains("not started")
        || status.contains("scheduled")
        || status.contains("pregame")
        || status == "0"
    {
        LiveGamePhase::Scheduled
    } else if status.contains("live")
        || status.contains("in progress")
        || status.contains("halftime")
        || status == "1"
    {
        LiveGamePhase::InProgress
    } else if status.contains("final") || status.contains("completed") || status == "2" {
        LiveGamePhase::Final
    } else {
        LiveGamePhase::Unknown
    }
}

fn parse_status_code(value: &Value) -> Option<i64> {
    let value = integer_value(value).ok()?;
    (0..=2).contains(&value).then_some(value)
}

fn normalize_players(
    body: &Map<String, Value>,
    game: &LiveGame,
) -> Result<Option<Vec<LivePlayerStats>>, NormalizeError> {
    let Some(section) = direct_value(body, &["playerStats"]) else {
        return Ok(None);
    };
    if section.is_null() {
        return Ok(None);
    }
    if !section.is_object() && !section.is_array() {
        return Err(NormalizeError::Invalid(
            "playerStats section is not a list or object".into(),
        ));
    }
    let mut rows = Vec::new();
    walk_player_records(section, None, &mut rows);
    let mut players = Vec::with_capacity(rows.len());
    for (hint, row) in rows {
        players.push(normalize_player(row, hint.as_deref(), game)?);
    }
    Ok(Some(players))
}

fn walk_player_records<'a>(
    value: &'a Value,
    hint: Option<&str>,
    rows: &mut Vec<(Option<String>, &'a Map<String, Value>)>,
) {
    match value {
        Value::Array(values) => {
            for value in values {
                walk_player_records(value, hint, rows);
            }
        }
        Value::Object(object) => {
            if looks_like_player_record(object) {
                rows.push((hint.map(str::to_owned), object));
                return;
            }
            for (key, child) in object {
                if matches!(child, Value::Object(_) | Value::Array(_)) {
                    let child_hint = is_id_hint(key).then_some(key.as_str());
                    walk_player_records(child, child_hint, rows);
                }
            }
        }
        _ => {}
    }
}

fn looks_like_player_record(object: &Map<String, Value>) -> bool {
    has_any_key(object, PLAYER_ID_KEYS)
        || has_any_key(object, PLAYER_NAME_KEYS)
        || object.keys().any(|key| {
            let normalized = normalize_key(key);
            PLAYER_SECTION_KEYS
                .iter()
                .any(|section| normalized == normalize_key(section))
                || PLAYER_STAT_KEYS
                    .iter()
                    .flat_map(|aliases| aliases.iter())
                    .any(|alias| normalized == normalize_key(alias))
        })
}

fn normalize_player(
    object: &Map<String, Value>,
    hint: Option<&str>,
    game: &LiveGame,
) -> Result<LivePlayerStats, NormalizeError> {
    let espn_id = player_id(object, hint)?;
    let name = optional_display(object, PLAYER_NAME_KEYS)?;
    let team = optional_team(object, PLAYER_TEAM_KEYS, game)?;
    let position = optional_position(object)?;
    let opponent = team.as_ref().map(|team| {
        if canonical_team(&game.home_team.0) == canonical_team(&team.0) {
            TeamAbbr(game.away_team.0.clone())
        } else {
            TeamAbbr(game.home_team.0.clone())
        }
    });

    let completions = metric_u32(object, COMPLETIONS, Some("passing"), None, "completions")?;
    let attempts = metric_u32(object, ATTEMPTS, Some("passing"), None, "attempts")?;
    let passing_tds = metric_u32(
        object,
        PASSING_TDS,
        Some("passing"),
        Some(&["td", "tds", "touchdowns", "touchdown"]),
        "passing_tds",
    )?;
    let passing_interceptions = metric_u32(
        object,
        PASSING_INTERCEPTIONS,
        Some("passing"),
        Some(&["int", "ints", "interceptions", "interception"]),
        "passing_interceptions",
    )?;
    let rushing_attempts = metric_u32(
        object,
        RUSHING_ATTEMPTS,
        Some("rushing"),
        None,
        "rushing_attempts",
    )?;
    let rushing_tds = metric_u32(
        object,
        RUSHING_TDS,
        Some("rushing"),
        Some(&["td", "tds", "touchdowns", "touchdown"]),
        "rushing_tds",
    )?;
    let targets = metric_u32(object, TARGETS, Some("receiving"), None, "targets")?;
    let receptions = metric_u32(
        object,
        RECEPTIONS,
        Some("receiving"),
        Some(&["receptions", "reception", "rec"]),
        "receptions",
    )?;
    let receiving_tds = metric_u32(
        object,
        RECEIVING_TDS,
        Some("receiving"),
        Some(&["td", "tds", "touchdowns", "touchdown"]),
        "receiving_tds",
    )?;
    let mut two_point_conversions = 0u32;
    let mut phase_present = false;
    for (aliases, context, name) in [
        (
            PASSING_TWO_POINT,
            "passing",
            "passing_two_point_conversions",
        ),
        (
            RUSHING_TWO_POINT,
            "rushing",
            "rushing_two_point_conversions",
        ),
        (
            RECEIVING_TWO_POINT,
            "receiving",
            "receiving_two_point_conversions",
        ),
    ] {
        let (value, present) =
            metric_u32_with_presence(object, aliases, Some(context), None, name)?;
        two_point_conversions = two_point_conversions
            .checked_add(value)
            .ok_or_else(|| NormalizeError::Invalid(format!("{name} exceeds u32")))?;
        phase_present |= present;
    }
    if !phase_present {
        two_point_conversions = metric_u32(object, TWO_POINT, None, None, "two_point_conversions")?;
    }

    Ok(LivePlayerStats {
        espn_id,
        name,
        team,
        position,
        opponent,
        completions,
        attempts,
        passing_tds,
        passing_interceptions,
        rushing_attempts,
        rushing_tds,
        targets,
        receptions,
        receiving_tds,
        fumbles_lost: metric_u32(object, FUMBLES_LOST, None, None, "fumbles_lost")?,
        two_point_conversions,
        special_teams_tds: metric_u32(object, SPECIAL_TEAMS_TDS, None, None, "special_teams_tds")?,
        fumble_recovery_tds: metric_u32(
            object,
            FUMBLE_RECOVERY_TDS,
            None,
            None,
            "fumble_recovery_tds",
        )?,
        passing_yards: metric_i32(
            object,
            PASSING_YARDS,
            Some("passing"),
            Some(&["yards", "yds"]),
            "passing_yards",
        )?,
        rushing_yards: metric_i32(
            object,
            RUSHING_YARDS,
            Some("rushing"),
            Some(&["yards", "yds"]),
            "rushing_yards",
        )?,
        receiving_yards: metric_i32(
            object,
            RECEIVING_YARDS,
            Some("receiving"),
            Some(&["yards", "yds"]),
            "receiving_yards",
        )?,
    })
}

fn player_id(
    object: &Map<String, Value>,
    hint: Option<&str>,
) -> Result<Option<EspnPlayerId>, NormalizeError> {
    let mut ids = BTreeSet::new();
    for value in direct_values(object, PLAYER_ID_KEYS) {
        if let Some(id) = nonempty_display(value)? {
            ids.insert(id);
        }
    }
    if ids.len() > 1 {
        return Err(NormalizeError::IdentityConflict(format!(
            "player identity fields disagree: {}",
            ids.into_iter().collect::<Vec<_>>().join(", ")
        )));
    }
    let explicit = ids.into_iter().next();
    if let (Some(hint), Some(explicit)) = (hint, explicit.as_deref())
        && hint != explicit
    {
        return Err(NormalizeError::IdentityConflict(format!(
            "player map id {hint:?} disagrees with player identity {explicit:?}"
        )));
    }
    Ok(explicit
        .or_else(|| hint.filter(|hint| !hint.is_empty()).map(str::to_owned))
        .map(EspnPlayerId))
}

fn normalize_defenses(
    body: &Map<String, Value>,
    game: &LiveGame,
) -> Result<Option<Vec<LiveTeamStats>>, NormalizeError> {
    let dst = direct_value(body, &["DST"]);
    let team_stats = direct_value(body, &["teamStats"]);
    if dst.is_none_or(Value::is_null) && team_stats.is_none_or(Value::is_null) {
        return Ok(None);
    }
    let dst_records = dst
        .map(|section| team_records(section, "DST"))
        .transpose()?
        .unwrap_or_default();
    let team_records = team_stats
        .map(|section| team_records(section, "teamStats"))
        .transpose()?
        .unwrap_or_default();
    let mut by_team: BTreeMap<String, TeamParts<'_>> = BTreeMap::new();
    for (hint, record) in dst_records {
        let team = record_team(record, hint.as_deref(), game)?;
        by_team.entry(team).or_default().dst.push(record);
    }
    for (hint, record) in team_records {
        let team = record_team(record, hint.as_deref(), game)?;
        by_team.entry(team).or_default().team_stats.push(record);
    }
    let mut defenses = Vec::with_capacity(by_team.len());
    for (canonical, parts) in by_team {
        let team = TeamAbbr(team_label_for_game(&canonical, game));
        let opponent = if canonical_team(&game.home_team.0) == Some(canonical.as_str()) {
            TeamAbbr(game.away_team.0.clone())
        } else {
            TeamAbbr(game.home_team.0.clone())
        };
        defenses.push(LiveTeamStats {
            team,
            opponent,
            sacks: merged_metric_u32(&parts, SACKS, "sacks")?,
            interceptions: merged_metric_u32(&parts, INTERCEPTIONS, "interceptions")?,
            fumble_recoveries: merged_metric_u32(&parts, FUMBLE_RECOVERIES, "fumble_recoveries")?,
            safeties: merged_metric_u32(&parts, SAFETIES, "safeties")?,
            touchdowns: merged_metric_u32(&parts, DEFENSIVE_TDS, "touchdowns")?,
            blocked_kicks: merged_sum_metric_u32(&parts, BLOCKED_KICKS, "blocked_kicks")?,
            conversion_returns: merged_sum_metric_u32(
                &parts,
                CONVERSION_RETURNS,
                "conversion_returns",
            )?,
            points_allowed: merged_metric_i32(&parts, POINTS_ALLOWED, "points_allowed")?,
            dst_present: !parts.dst.is_empty(),
            team_stats_present: !parts.team_stats.is_empty(),
        });
    }
    Ok(Some(defenses))
}

#[derive(Default)]
struct TeamParts<'a> {
    dst: Vec<&'a Map<String, Value>>,
    team_stats: Vec<&'a Map<String, Value>>,
}
type TeamRecord<'a> = (Option<String>, &'a Map<String, Value>);

fn team_records<'a>(section: &'a Value, name: &str) -> Result<Vec<TeamRecord<'a>>, NormalizeError> {
    let mut records = Vec::new();
    walk_team_records(section, None, &mut records);
    if records.is_empty()
        && !matches!(section, Value::Object(object) if object.is_empty())
        && !matches!(section, Value::Array(values) if values.is_empty())
    {
        return Err(NormalizeError::Invalid(format!(
            "{name} contained no team records"
        )));
    }
    Ok(records)
}

fn walk_team_records<'a>(value: &'a Value, hint: Option<&str>, records: &mut Vec<TeamRecord<'a>>) {
    match value {
        Value::Array(values) => {
            for value in values {
                walk_team_records(value, hint, records);
            }
        }
        Value::Object(object) => {
            if looks_like_team_record(object) {
                records.push((hint.map(str::to_owned), object));
            } else {
                for (key, child) in object {
                    if matches!(child, Value::Object(_) | Value::Array(_)) {
                        walk_team_records(child, Some(key), records);
                    }
                }
            }
        }
        _ => {}
    }
}

fn looks_like_team_record(object: &Map<String, Value>) -> bool {
    has_any_key(object, PLAYER_TEAM_KEYS)
        || object.keys().any(|key| {
            let normalized = normalize_key(key);
            DST_STAT_KEYS
                .iter()
                .flat_map(|aliases| aliases.iter())
                .any(|alias| normalized == normalize_key(alias))
        })
}

fn record_team(
    object: &Map<String, Value>,
    hint: Option<&str>,
    game: &LiveGame,
) -> Result<String, NormalizeError> {
    let mut candidates = Vec::new();
    for value in direct_values(object, PLAYER_TEAM_KEYS) {
        if let Some(value) = nonempty_display(value)? {
            candidates.push(value);
        }
    }
    if let Some(hint) = hint
        && !is_structural_team_hint(hint)
        && (candidates.is_empty() || canonical_team(hint).is_some())
    {
        candidates.push(hint.to_owned());
    }
    let mut normalized = BTreeSet::new();
    for candidate in candidates {
        normalized.insert(canonical_team_for_game(&candidate, game)?);
    }
    match normalized.len() {
        0 => Err(NormalizeError::Invalid(
            "team record has no team identity".into(),
        )),
        1 => Ok(normalized.into_iter().next().unwrap().into()),
        _ => Err(NormalizeError::IdentityConflict(
            "team record has ambiguous team identity".into(),
        )),
    }
}
fn is_structural_team_hint(value: &str) -> bool {
    matches!(
        normalize_key(value).as_str(),
        "home" | "away" | "hometeam" | "awayteam"
    )
}

fn merged_metric_u32(
    parts: &TeamParts<'_>,
    aliases: &[&str],
    name: &str,
) -> Result<u32, NormalizeError> {
    if let Some(value) = source_metric_u32(&parts.dst, aliases, name)? {
        return Ok(value);
    }
    Ok(source_metric_u32(&parts.team_stats, aliases, name)?.unwrap_or(0))
}

fn merged_sum_metric_u32(
    parts: &TeamParts<'_>,
    aliases: &[&str],
    name: &str,
) -> Result<u32, NormalizeError> {
    if let Some(value) = source_sum_metric_u32(&parts.dst, aliases, name)? {
        return Ok(value);
    }
    Ok(source_sum_metric_u32(&parts.team_stats, aliases, name)?.unwrap_or(0))
}

fn merged_metric_i32(
    parts: &TeamParts<'_>,
    aliases: &[&str],
    name: &str,
) -> Result<i32, NormalizeError> {
    if let Some(value) = source_metric_i32(&parts.dst, aliases, name)? {
        return Ok(value);
    }
    Ok(source_metric_i32(&parts.team_stats, aliases, name)?.unwrap_or(0))
}

fn source_metric_u32(
    records: &[&Map<String, Value>],
    aliases: &[&str],
    name: &str,
) -> Result<Option<u32>, NormalizeError> {
    for record in records {
        let (value, present) = metric_u32_with_presence(record, aliases, None, None, name)?;
        if present {
            return Ok(Some(value));
        }
    }
    Ok(None)
}

fn source_sum_metric_u32(
    records: &[&Map<String, Value>],
    aliases: &[&str],
    name: &str,
) -> Result<Option<u32>, NormalizeError> {
    let mut total = 0u32;
    let mut present = false;
    let mut seen_aliases = BTreeSet::new();
    for record in records {
        let rows = metric_rows(record, aliases, None, None);
        for (key, value) in rows {
            let normalized = normalize_key(&key);
            if !seen_aliases.insert(normalized) {
                continue;
            }
            total = total
                .checked_add(parse_u32(value, name)?)
                .ok_or_else(|| NormalizeError::Invalid(format!("{name} exceeds u32")))?;
            present = true;
        }
    }
    Ok(present.then_some(total))
}

fn source_metric_i32(
    records: &[&Map<String, Value>],
    aliases: &[&str],
    name: &str,
) -> Result<Option<i32>, NormalizeError> {
    for record in records {
        let rows = metric_rows(record, aliases, None, None);
        if let Some((_, value)) = rows.into_iter().next() {
            return Ok(Some(parse_i32(value, name)?));
        }
    }
    Ok(None)
}

fn metric_u32(
    object: &Map<String, Value>,
    aliases: &[&str],
    context: Option<&str>,
    generic: Option<&[&str]>,
    name: &str,
) -> Result<u32, NormalizeError> {
    Ok(metric_u32_with_presence(object, aliases, context, generic, name)?.0)
}

fn metric_u32_with_presence(
    object: &Map<String, Value>,
    aliases: &[&str],
    context: Option<&str>,
    generic: Option<&[&str]>,
    name: &str,
) -> Result<(u32, bool), NormalizeError> {
    let rows = metric_rows(object, aliases, context, generic);
    let Some((_, value)) = rows.into_iter().next() else {
        return Ok((0, false));
    };
    Ok((parse_u32(value, name)?, true))
}

fn metric_i32(
    object: &Map<String, Value>,
    aliases: &[&str],
    context: Option<&str>,
    generic: Option<&[&str]>,
    name: &str,
) -> Result<i32, NormalizeError> {
    let rows = metric_rows(object, aliases, context, generic);
    let Some((_, value)) = rows.into_iter().next() else {
        return Ok(0);
    };
    parse_i32(value, name)
}

fn metric_rows<'a>(
    object: &'a Map<String, Value>,
    aliases: &[&str],
    context: Option<&str>,
    generic: Option<&[&str]>,
) -> Vec<(String, &'a Value)> {
    let mut rows = Vec::new();
    for (key, value) in object {
        flatten_scalars(value, key.clone(), &mut rows);
    }
    let wanted = aliases
        .iter()
        .map(|alias| normalize_key(alias))
        .collect::<BTreeSet<_>>();
    let exact = rows
        .iter()
        .filter(|(path, key, _)| wanted.contains(key) && path_matches_context(path, context))
        .map(|(_, key, value)| (key.clone(), *value))
        .collect::<Vec<_>>();
    if !exact.is_empty() {
        return exact;
    }
    let Some(generic) = generic else {
        return Vec::new();
    };
    let generic = generic
        .iter()
        .map(|alias| normalize_key(alias))
        .collect::<BTreeSet<_>>();
    rows.into_iter()
        .filter(|(path, key, _)| generic.contains(key) && path_matches_context(path, context))
        .map(|(_, key, value)| (key, value))
        .collect()
}

fn flatten_scalars<'a>(
    value: &'a Value,
    path: String,
    rows: &mut Vec<(String, String, &'a Value)>,
) {
    match value {
        Value::Object(object) => {
            for (key, child) in object {
                let child_path = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                flatten_scalars(child, child_path, rows);
            }
        }
        Value::Array(values) => {
            for (index, child) in values.iter().enumerate() {
                flatten_scalars(child, format!("{path}[{index}]"), rows);
            }
        }
        _ => rows.push((
            path.clone(),
            normalize_key(path.rsplit('.').next().unwrap_or(&path)),
            value,
        )),
    }
}

fn path_contains(path: &str, context: &str) -> bool {
    let context = normalize_key(context);
    path.split(['.', '[', ']'])
        .any(|part| normalize_key(part) == context)
}
fn path_matches_context(path: &str, context: Option<&str>) -> bool {
    context.is_none_or(|context| path_contains(path, context) || is_top_level_path(path))
}

fn is_top_level_path(path: &str) -> bool {
    !path.contains('.') && !path.contains('[')
}

fn parse_u32(value: &Value, name: &str) -> Result<u32, NormalizeError> {
    let value = integer_value(value)?;
    if value < 0 {
        return Err(NormalizeError::Invalid(format!(
            "known count {name} cannot be negative"
        )));
    }
    u32::try_from(value).map_err(|_| NormalizeError::Invalid(format!("{name} exceeds u32")))
}

fn parse_i32(value: &Value, name: &str) -> Result<i32, NormalizeError> {
    let value = integer_value(value)?;
    i32::try_from(value).map_err(|_| NormalizeError::Invalid(format!("{name} exceeds i32")))
}

fn integer_value(value: &Value) -> Result<i64, NormalizeError> {
    match value {
        Value::Null => Ok(0),
        Value::Bool(_) => Err(NormalizeError::Invalid(
            "boolean is not an integer-valued statistic".into(),
        )),
        Value::Number(number) => {
            if let Some(value) = number.as_i64() {
                Ok(value)
            } else if let Some(value) = number.as_u64() {
                i64::try_from(value)
                    .map_err(|_| NormalizeError::Invalid("numeric statistic exceeds i64".into()))
            } else if let Some(value) = number.as_f64() {
                if value.is_finite()
                    && value.fract() == 0.0
                    && value >= i64::MIN as f64
                    && value <= i64::MAX as f64
                {
                    Ok(value as i64)
                } else {
                    Err(NormalizeError::Invalid(
                        "statistic is not integer-valued".into(),
                    ))
                }
            } else {
                Err(NormalizeError::Invalid("statistic is not numeric".into()))
            }
        }
        Value::String(text) => {
            let text = text.trim();
            if text.is_empty() || matches!(text, "-" | "--" | "N/A" | "n/a") {
                return Ok(0);
            }
            if let Ok(value) = text.parse::<i64>() {
                return Ok(value);
            }
            if let Ok(value) = text.parse::<f64>()
                && value.is_finite()
                && value.fract() == 0.0
                && value >= i64::MIN as f64
                && value <= i64::MAX as f64
            {
                return Ok(value as i64);
            }
            Err(NormalizeError::Invalid(format!(
                "malformed numeric value {text:?}"
            )))
        }
        Value::Array(_) | Value::Object(_) => Err(NormalizeError::Invalid(
            "object or list is not an integer-valued statistic".into(),
        )),
    }
}

fn optional_score(
    object: &Map<String, Value>,
    aliases: &[&str],
) -> Result<Option<i32>, NormalizeError> {
    let Some(value) = direct_value(object, aliases) else {
        return Ok(None);
    };
    if matches!(value, Value::Null) || is_empty_marker(value) {
        return Ok(None);
    }
    Ok(Some(parse_i32(value, "score")?))
}

fn optional_display(
    object: &Map<String, Value>,
    aliases: &[&str],
) -> Result<Option<String>, NormalizeError> {
    let Some(value) = direct_value(object, aliases) else {
        return Ok(None);
    };
    if matches!(value, Value::Null) || is_empty_marker(value) {
        return Ok(None);
    }
    Ok(Some(display_value(value)?))
}

fn optional_position(object: &Map<String, Value>) -> Result<Option<String>, NormalizeError> {
    let Some(position) = optional_display(object, PLAYER_POSITION_KEYS)? else {
        return Ok(None);
    };
    let normalized = normalize_key(&position).to_ascii_uppercase();
    let normalized = match normalized.as_str() {
        "WIDERECEIVER" => "WR",
        "RUNNINGBACK" => "RB",
        "QUARTERBACK" => "QB",
        "TIGHTEND" => "TE",
        "FULLBACK" => "FB",
        "DEFENSIVEEND" => "DE",
        "DEFENSIVETACKLE" => "DT",
        "LINEBACKER" => "LB",
        "CORNERBACK" => "CB",
        "SAFETY" => "S",
        _ => normalized.as_str(),
    };
    Ok(Some(normalized.to_owned()))
}

fn optional_team(
    object: &Map<String, Value>,
    aliases: &[&str],
    game: &LiveGame,
) -> Result<Option<TeamAbbr>, NormalizeError> {
    let values = direct_values(object, aliases);
    if values.is_empty() {
        return Ok(None);
    }
    let mut teams = BTreeSet::new();
    for value in values {
        if let Some(value) = team_display(value, "player team")? {
            teams.insert(team_for_game(&value, game)?);
        }
    }
    match teams.len() {
        0 => Ok(None),
        1 => Ok(Some(TeamAbbr(teams.into_iter().next().unwrap()))),
        _ => Err(NormalizeError::IdentityConflict(
            "player team fields disagree".into(),
        )),
    }
}

fn scoreboard_team(
    object: &Map<String, Value>,
    aliases: &[&str],
    side: &str,
) -> Result<TeamAbbr, NormalizeError> {
    let value = direct_value(object, aliases)
        .ok_or_else(|| NormalizeError::Invalid(format!("scoreboard game has no {side} team")))?;
    let value = team_display(value, side)?.ok_or_else(|| {
        NormalizeError::Invalid(format!("scoreboard game has an empty {side} team"))
    })?;
    let value = canonical_team(&value)
        .ok_or_else(|| NormalizeError::Invalid(format!("unknown team abbreviation {value:?}")))?;
    Ok(TeamAbbr(value.into()))
}

fn team_display(value: &Value, field: &str) -> Result<Option<String>, NormalizeError> {
    if let Some(object) = value.as_object() {
        let value = direct_scalar(
            object,
            &["teamAbv", "teamAbbr", "abbreviation", "abbr", "name", "id"],
        )
        .ok_or_else(|| NormalizeError::Invalid(format!("{field} object has no scalar value")))?;
        return nonempty_display(value);
    }
    nonempty_display(value)
}

fn team_for_game(value: &str, game: &LiveGame) -> Result<String, NormalizeError> {
    let value = canonical_team_for_game(value, game)?;
    Ok(team_label_for_game(value, game))
}

fn canonical_team_for_game(value: &str, game: &LiveGame) -> Result<&'static str, NormalizeError> {
    let value = canonical_team(value)
        .ok_or_else(|| NormalizeError::Invalid(format!("unknown team abbreviation {value:?}")))?;
    let home = canonical_team(&game.home_team.0).ok_or_else(|| {
        NormalizeError::Invalid(format!(
            "unknown requested home team {:?}",
            game.home_team.0
        ))
    })?;
    let away = canonical_team(&game.away_team.0).ok_or_else(|| {
        NormalizeError::Invalid(format!(
            "unknown requested away team {:?}",
            game.away_team.0
        ))
    })?;
    if value != home && value != away {
        return Err(NormalizeError::Invalid(format!(
            "team {value:?} is not one of requested game teams"
        )));
    }
    Ok(value)
}

fn team_label_for_game(value: &str, game: &LiveGame) -> String {
    if canonical_team(&game.home_team.0) == Some(value) {
        game.home_team.0.clone()
    } else {
        game.away_team.0.clone()
    }
}

fn canonical_team(value: &str) -> Option<&'static str> {
    let normalized = normalize_key(value).to_ascii_uppercase();
    match normalized.as_str() {
        "ARI" => Some("ARI"),
        "ATL" => Some("ATL"),
        "BAL" => Some("BAL"),
        "BUF" => Some("BUF"),
        "CAR" => Some("CAR"),
        "CHI" => Some("CHI"),
        "CIN" => Some("CIN"),
        "CLE" => Some("CLE"),
        "DAL" => Some("DAL"),
        "DEN" => Some("DEN"),
        "DET" => Some("DET"),
        "GB" => Some("GB"),
        "HOU" => Some("HOU"),
        "IND" => Some("IND"),
        "JAX" | "JAC" => Some("JAX"),
        "KC" => Some("KC"),
        "LAC" | "SD" | "SDG" => Some("LAC"),
        "LAR" | "LA" => Some("LA"),
        "LV" | "OAK" => Some("LV"),
        "MIA" => Some("MIA"),
        "MIN" => Some("MIN"),
        "NE" => Some("NE"),
        "NO" => Some("NO"),
        "NYG" => Some("NYG"),
        "NYJ" => Some("NYJ"),
        "PHI" => Some("PHI"),
        "PIT" => Some("PIT"),
        "SEA" => Some("SEA"),
        "SF" | "SFO" => Some("SF"),
        "TB" | "TAM" => Some("TB"),
        "TEN" => Some("TEN"),
        "WAS" | "WSH" => Some("WAS"),
        _ => None,
    }
}

fn direct_value<'a>(object: &'a Map<String, Value>, aliases: &[&str]) -> Option<&'a Value> {
    let wanted = aliases
        .iter()
        .map(|alias| normalize_key(alias))
        .collect::<BTreeSet<_>>();
    object
        .iter()
        .find(|(key, _)| wanted.contains(&normalize_key(key)))
        .map(|(_, value)| value)
}
fn direct_string(
    object: &Map<String, Value>,
    aliases: &[&str],
) -> Result<Option<String>, NormalizeError> {
    direct_value(object, aliases)
        .map(nonempty_display)
        .transpose()
        .map(|value| value.flatten())
}

fn direct_values<'a>(object: &'a Map<String, Value>, aliases: &[&str]) -> Vec<&'a Value> {
    let wanted = aliases
        .iter()
        .map(|alias| normalize_key(alias))
        .collect::<BTreeSet<_>>();
    object
        .iter()
        .filter(|(key, _)| wanted.contains(&normalize_key(key)))
        .map(|(_, value)| value)
        .collect()
}

fn direct_scalar<'a>(object: &'a Map<String, Value>, aliases: &[&str]) -> Option<&'a Value> {
    direct_value(object, aliases).filter(|value| !value.is_object() && !value.is_array())
}

fn display_value(value: &Value) -> Result<String, NormalizeError> {
    match value {
        Value::String(value) => Ok(value.clone()),
        Value::Number(value) => Ok(value.to_string()),
        Value::Bool(value) => Ok(value.to_string()),
        Value::Null => Err(NormalizeError::Invalid("null has no display value".into())),
        Value::Array(_) | Value::Object(_) => Err(NormalizeError::Invalid(
            "display field is not scalar".into(),
        )),
    }
}

fn display_value_opt(value: &Value) -> Option<String> {
    display_value(value)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

fn nonempty_display(value: &Value) -> Result<Option<String>, NormalizeError> {
    if value.is_null() || is_empty_marker(value) {
        return Ok(None);
    }
    let value = display_value(value)?;
    Ok((!value.trim().is_empty()).then_some(value))
}

fn is_empty_marker(value: &Value) -> bool {
    value
        .as_str()
        .is_some_and(|value| matches!(value.trim(), "" | "-" | "--" | "N/A" | "n/a"))
}

fn has_any_key(object: &Map<String, Value>, aliases: &[&str]) -> bool {
    aliases.iter().any(|alias| {
        object
            .keys()
            .any(|key| normalize_key(key) == normalize_key(alias))
    })
}

fn is_id_hint(value: &str) -> bool {
    !value.is_empty() && value.chars().all(|character| character.is_ascii_digit())
}

fn normalize_key(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn is_wrapper_metadata(key: &str) -> bool {
    matches!(
        normalize_key(key).as_str(),
        "statuscode" | "status" | "message" | "error" | "success" | "data"
    )
}

fn is_stable_section(key: &str) -> bool {
    matches!(
        normalize_key(key).as_str(),
        "playerstats" | "dst" | "teamstats" | "scoringplays" | "allplaybyplay"
    )
}

fn is_volatile_metadata(key: &str) -> bool {
    matches!(
        normalize_key(key).as_str(),
        "gamestatus"
            | "gamestatuscode"
            | "currentperiod"
            | "quarter"
            | "phase"
            | "period"
            | "clock"
            | "gameclock"
            | "time"
            | "status"
            | "statuscode"
            | "score"
            | "homescore"
            | "awayscore"
            | "homepoints"
            | "awaypoints"
            | "homepts"
            | "awaypts"
            | "lastplay"
            | "updatedat"
            | "timestamp"
            | "requestedat"
            | "receivedat"
            | "elapsedms"
            | "requesttiming"
            | "sequence"
            | "requestid"
            | "gametime"
    )
}

fn canonicalize(value: &Value) -> Value {
    match value {
        Value::Object(object) => Value::Object(
            object
                .iter()
                .map(|(key, value)| (key.clone(), canonicalize(value)))
                .collect::<BTreeMap<_, _>>()
                .into_iter()
                .collect(),
        ),
        Value::Array(values) => Value::Array(values.iter().map(canonicalize).collect()),
        value => value.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nfl_data::{LiveGame, LiveGamePhase, Season, SeasonType, TeamAbbr, Week};
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
            home_team: TeamAbbr("LAR".into()),
            away_team: TeamAbbr("SF".into()),
        }
    }

    #[test]
    fn normalizes_observed_box_aliases_and_omitted_zeros() {
        let value = json!({
            "gameID": "20260910_SF@LAR",
            "gameStatus": "Halftime",
            "currentPeriod": "2nd",
            "gameClock": "00:00",
            "homePts": "7",
            "awayPts": 3,
            "playerStats": {
                "123": {
                    "longName": "Player",
                    "playerID": "123",
                    "teamAbv": "SF",
                    "Passing": {"passCompletions": "2", "passAttempts": "4", "passYds": "-3", "passTD": "1", "int": "0"},
                    "Rushing": {"carries": "", "rushYds": "--", "rushTD": null},
                    "Receiving": {"targets": "N/A", "receptions": "1", "recYds": 8, "recTD": 0}
                }
            }
        });
        let snapshot =
            normalize_box_score(&value, &game(), datetime!(2026-09-11 01:00:00 UTC)).unwrap();
        assert_eq!(snapshot.phase, LiveGamePhase::InProgress);
        assert_eq!(snapshot.home_score, Some(7));
        let player = &snapshot.players.unwrap()[0];
        assert_eq!(player.completions, 2);
        assert_eq!(player.attempts, 4);
        assert_eq!(player.passing_yards, -3);
        assert_eq!(player.rushing_attempts, 0);
        assert_eq!(player.targets, 0);
        assert_eq!(player.receiving_yards, 8);
        assert_eq!(player.passing_tds, 1);
        assert_eq!(player.rushing_tds, 0);
        assert_eq!(player.receiving_tds, 0);
    }
    #[test]
    fn falls_back_to_flat_exact_metric_aliases_without_context() {
        let value = json!({
            "gameID": "20260910_SF@LAR",
            "playerStats": [{
                "playerID": "1",
                "team": "SF",
                "passYds": "42",
                "rushYds": "-3"
            }]
        });
        let snapshot =
            normalize_box_score(&value, &game(), datetime!(2026-09-11 01:00:00 UTC)).unwrap();
        let player = &snapshot.players.unwrap()[0];
        assert_eq!(player.passing_yards, 42);
        assert_eq!(player.rushing_yards, -3);
    }

    #[test]
    fn does_not_bleed_generic_metrics_between_categories() {
        let value = json!({
            "gameID": "20260910_SF@LAR",
            "playerStats": [{
                "playerID": "1",
                "team": "SF",
                "Passing": {},
                "Rushing": {},
                "Receiving": {"td": "1"},
                "Defense": {"int": "2"}
            }]
        });
        let snapshot =
            normalize_box_score(&value, &game(), datetime!(2026-09-11 01:00:00 UTC)).unwrap();
        let player = &snapshot.players.unwrap()[0];
        assert_eq!(player.passing_tds, 0);
        assert_eq!(player.passing_interceptions, 0);
        assert_eq!(player.rushing_tds, 0);
        assert_eq!(player.receiving_tds, 1);
    }

    #[test]
    fn keeps_requested_game_team_spelling_across_provider_aliases() {
        let mut game = game();
        game.home_team = TeamAbbr("LA".into());
        let value = json!({
            "gameID": "20260910_SF@LAR",
            "playerStats": [{
                "playerID": "1",
                "team": "LAR",
                "Rushing": {"rushYds": "4"}
            }],
            "DST": {"home": {"teamAbv": "LAR", "defensiveInterceptions": "1"}},
            "teamStats": {"home": {"team": "LAR", "ptsAllowed": "7"}}
        });
        let snapshot =
            normalize_box_score(&value, &game, datetime!(2026-09-11 01:00:00 UTC)).unwrap();
        let player = &snapshot.players.unwrap()[0];
        assert_eq!(player.team, Some(TeamAbbr("LA".into())));
        assert_eq!(player.opponent, Some(TeamAbbr("SF".into())));
        let defense = &snapshot.defenses.unwrap()[0];
        assert_eq!(defense.team, TeamAbbr("LA".into()));
        assert_eq!(defense.opponent, TeamAbbr("SF".into()));
        assert_eq!(defense.interceptions, 1);
    }
    #[test]
    fn normalizes_trimmed_synthetic_tank01_capture() {
        let mut game = game();
        game.home_team = TeamAbbr("LA".into());
        let mut final_snapshot = None;
        let mut saw_play_by_play = false;
        for line in include_str!("../tests/fixtures/tank01_box_bodies.ndjson").lines() {
            let record: Value = serde_json::from_str(line).unwrap();
            let kind = record["kind"].as_str().unwrap();
            let body = &record["body"];
            if kind == "pregame" {
                assert!(pregame_message(body).is_some());
                continue;
            }
            if kind == "play_by_play" {
                saw_play_by_play = body["allPlayByPlay"]
                    .as_array()
                    .is_some_and(|plays| !plays.is_empty());
            }
            let snapshot =
                normalize_box_score(body, &game, datetime!(2026-09-11 01:00:00 UTC)).unwrap();
            if kind == "final" {
                final_snapshot = Some(snapshot);
            } else {
                assert_eq!(snapshot.phase, LiveGamePhase::InProgress);
            }
        }
        assert!(saw_play_by_play);
        let snapshot = final_snapshot.expect("fixture must contain a final snapshot");
        assert_eq!(snapshot.phase, LiveGamePhase::Final);
        assert_eq!(snapshot.home_score, Some(7));
        assert_eq!(snapshot.away_score, Some(27));

        let players = snapshot.players.unwrap();
        let stafford = players
            .iter()
            .find(|player| {
                player
                    .espn_id
                    .as_ref()
                    .is_some_and(|espn_id| espn_id.0 == "12483")
            })
            .unwrap();
        assert_eq!(stafford.passing_yards, 155);
        assert_eq!(stafford.rushing_yards, -1);
        assert_eq!(stafford.attempts, 25);
        assert_eq!(stafford.completions, 15);
        assert_eq!(stafford.passing_interceptions, 1);

        let evans = players
            .iter()
            .find(|player| {
                player
                    .espn_id
                    .as_ref()
                    .is_some_and(|espn_id| espn_id.0 == "16737")
            })
            .unwrap();
        assert_eq!(evans.receiving_yards, 49);
        assert_eq!(evans.receptions, 6);
        assert_eq!(evans.receiving_tds, 1);

        let defenses = snapshot.defenses.unwrap();
        let sf = defenses
            .iter()
            .find(|defense| defense.team.0 == "SF")
            .unwrap();
        assert_eq!(sf.interceptions, 1);
        assert_eq!(sf.fumble_recoveries, 1);
        assert_eq!(sf.points_allowed, 7);
        assert_eq!(sf.interceptions * 2 + sf.fumble_recoveries * 2 + 4, 8);

        let la = defenses
            .iter()
            .find(|defense| defense.team.0 == "LA")
            .unwrap();
        assert_eq!(la.interceptions, 1);
        assert_eq!(la.points_allowed, 27);
        assert_eq!(la.interceptions * 2, 2);
    }

    #[test]
    fn preserves_absent_and_present_empty_sections() {
        let absent = normalize_box_score(
            &json!({"gameID":"20260910_SF@LAR"}),
            &game(),
            datetime!(2026-09-11 01:00:00 UTC),
        )
        .unwrap();
        assert_eq!(absent.players, None);
        assert_eq!(absent.defenses, None);
        let empty = normalize_box_score(
            &json!({"gameID":"20260910_SF@LAR", "playerStats": {}, "DST": [], "teamStats": {}}),
            &game(),
            datetime!(2026-09-11 01:00:00 UTC),
        )
        .unwrap();
        assert_eq!(empty.players, Some(Vec::new()));
        assert_eq!(empty.defenses, Some(Vec::new()));
    }

    #[test]
    fn rejects_negative_counts_and_conflicting_identity() {
        let negative = json!({"gameID":"20260910_SF@LAR", "playerStats":[{"playerID":"1", "team":"SF", "Passing":{"passAttempts":"-1"}}]});
        assert!(matches!(
            normalize_box_score(&negative, &game(), datetime!(2026-09-11 01:00:00 UTC)),
            Err(NormalizeError::Invalid(_))
        ));
        let conflict = json!({"gameID":"20260910_SF@LAR", "playerStats":[{"playerID":"1", "espnId":"2", "team":"SF"}]});
        assert!(matches!(
            normalize_box_score(&conflict, &game(), datetime!(2026-09-11 01:00:00 UTC)),
            Err(NormalizeError::IdentityConflict(_))
        ));
        let map_conflict = json!({
            "gameID": "20260910_SF@LAR",
            "playerStats": {"1": {"playerID": "2", "team": "SF"}}
        });
        assert!(matches!(
            normalize_box_score(&map_conflict, &game(), datetime!(2026-09-11 01:00:00 UTC)),
            Err(NormalizeError::IdentityConflict(_))
        ));
        let wrong_game = json!({
            "OTHER_GAME": {
                "playerStats": [{"playerID": "1", "team": "SF"}]
            },
            "OTHER_GAME_2": {
                "DST": []
            }
        });
        assert!(matches!(
            normalize_box_score(&wrong_game, &game(), datetime!(2026-09-11 01:00:00 UTC)),
            Err(NormalizeError::IdentityConflict(_))
        ));
    }

    #[test]
    fn merges_dst_over_team_stats_without_player_defender_aggregation() {
        let value = json!({
            "gameID": "20260910_SF@LAR",
            "playerStats": [{"playerID":"99", "team":"SF", "Defense":{"sacks":"99"}}],
            "DST": {"away": {"teamAbv":"SF", "sacks":"1", "defensiveInterceptions":"2", "blockedFG":"1"}},
            "teamStats": {"away": {"teamAbv":"SF", "sacks":"9", "fumblesRecovered":"3", "blockedPunt":"2"}}
        });
        let snapshot =
            normalize_box_score(&value, &game(), datetime!(2026-09-11 01:00:00 UTC)).unwrap();
        let defense = &snapshot.defenses.unwrap()[0];
        assert_eq!(defense.sacks, 1);
        assert_eq!(defense.interceptions, 2);
        assert_eq!(defense.fumble_recoveries, 3);
        assert_eq!(defense.blocked_kicks, 1);
        assert!(defense.dst_present && defense.team_stats_present);
    }
    #[test]
    fn rejects_conflicting_defense_map_identity() {
        let value = json!({
            "gameID": "20260910_SF@LAR",
            "DST": {"SF": {"team": "LAR", "sacks": "1"}}
        });
        assert!(matches!(
            normalize_box_score(&value, &game(), datetime!(2026-09-11 01:00:00 UTC)),
            Err(NormalizeError::IdentityConflict(_))
        ));
    }

    #[test]
    fn maps_scoreboard_status_and_unknown_status() {
        let scoreboard = normalize_scoreboard(&json!({
            "a": {"gameID":"a", "home":"LAR", "away":"SF", "gameStatusCode":"0"},
            "b": {"gameID":"b", "home":"KC", "away":"BUF", "gameStatus":"mystery"}
        }))
        .unwrap();
        assert_eq!(scoreboard.games[0].phase, LiveGamePhase::Scheduled);
        assert_eq!(scoreboard.games[1].phase, LiveGamePhase::Unknown);
        let nested = normalize_scoreboard(&json!({
            "games": [{
                "gameID": "c",
                "homeTeam": {"abbreviation": "LA"},
                "awayTeam": {"teamAbbr": "SFO"},
                "status": "scheduled"
            }]
        }))
        .unwrap();
        assert_eq!(nested.games[0].home_team, TeamAbbr("LA".into()));
        assert_eq!(nested.games[0].away_team, TeamAbbr("SF".into()));
        assert!(
            normalize_scoreboard(&json!({"games": []}))
                .unwrap()
                .games
                .is_empty()
        );
        assert!(matches!(
            normalize_scoreboard(&json!({"error": "quota exceeded"})),
            Err(NormalizeError::Invalid(_))
        ));
        assert!(matches!(
            normalize_scoreboard(&json!({
                "a": {"gameID": "b", "home": "LAR", "away": "SF"}
            })),
            Err(NormalizeError::IdentityConflict(_))
        ));
    }

    #[test]
    fn stable_projection_ignores_clock_but_keeps_stats_and_unknown_fields() {
        let first = json!({
            "gameStatus": "Live",
            "gameClock": "10:00",
            "phase": "1",
            "quarter": "1",
            "playerStats": {"1": {"rushing": {"rushYds": "4"}}},
            "scoringPlays": [{"id": 1}],
            "allPlayByPlay": [{"id": 1}],
            "unknown": "kept"
        });
        let clock = json!({
            "gameStatus": "Live",
            "gameClock": "09:00",
            "phase": "2",
            "quarter": "2",
            "playerStats": {"1": {"rushing": {"rushYds": "4"}}},
            "scoringPlays": [{"id": 1}],
            "allPlayByPlay": [{"id": 1}],
            "unknown": "changed-but-excluded"
        });
        let stat = json!({
            "gameStatus": "Live",
            "gameClock": "09:00",
            "playerStats": {"1": {"rushing": {"rushYds": "5"}}},
            "scoringPlays": [{"id": 1}],
            "allPlayByPlay": [{"id": 1}]
        });
        let scoring = json!({
            "gameStatus": "Live",
            "gameClock": "09:00",
            "playerStats": {"1": {"rushing": {"rushYds": "4"}}},
            "scoringPlays": [{"id": 2}],
            "allPlayByPlay": [{"id": 1}]
        });
        let play = json!({
            "gameStatus": "Live",
            "gameClock": "09:00",
            "playerStats": {"1": {"rushing": {"rushYds": "4"}}},
            "scoringPlays": [{"id": 1}],
            "allPlayByPlay": [{"id": 2}]
        });
        let volatile_first = json!({
            "gameStatus": "Live",
            "gameClock": "10:00",
            "phase": "1",
            "quarter": "1",
            "homePts": "7",
            "awayPts": "3",
            "rawUnknown": {"a": 1}
        });
        let volatile_changed = json!({
            "gameStatus": "Completed",
            "gameClock": "",
            "phase": "Final",
            "quarter": "4",
            "homePts": "8",
            "awayPts": "3",
            "rawUnknown": {"a": 1}
        });
        assert_eq!(
            stable_projection(&volatile_first),
            stable_projection(&volatile_changed)
        );
        assert_eq!(stable_projection(&first), stable_projection(&clock));
        assert_ne!(stable_projection(&first), stable_projection(&stat));
        assert_ne!(stable_projection(&first), stable_projection(&scoring));
        assert_ne!(stable_projection(&first), stable_projection(&play));
        let fallback =
            stable_projection(&json!({"status":"Live", "clock":"1", "rawUnknown": {"b":1,"a":2}}));
        assert_eq!(fallback["rawUnknown"]["a"], 2);
    }

    #[test]
    fn recognizes_pregame_error() {
        assert_eq!(
            pregame_message_for(
                &json!({
                    "20260910_SF@LAR": {
                        "error": "Game hasn't started yet, it will start at 8:35p(ET)."
                    }
                }),
                Some("20260910_SF@LAR")
            )
            .unwrap(),
            Some("Game hasn't started yet, it will start at 8:35p(ET).".into())
        );
        assert_eq!(
            pregame_message(
                &json!({"error":"Game hasn't started yet, it will start at 8:35p(ET)."})
            ),
            Some("Game hasn't started yet, it will start at 8:35p(ET).".into())
        );
        assert_eq!(
            pregame_message(&json!({"error":"unknown provider failure"})),
            None
        );
    }

    #[test]
    fn date_is_serializable_for_live_contract() {
        let slate = nfl_data::LiveSlate {
            date: date!(2026 - 09 - 10),
            games: vec![game()],
        };
        let encoded = serde_json::to_string(&slate).unwrap();
        assert!(encoded.contains("2026-09-10"));
    }
}
