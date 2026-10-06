use std::{borrow::Cow, collections::BTreeMap, time::Instant};

use nfl_data::{
    LiveGame, LiveGameSnapshot, LiveProviderError, LiveScoreProvider, LiveScoreboard,
    ProviderOutcome, ProviderResponse, RawBody,
};
use reqwest::{Url, header::HeaderMap};
use serde_json::Value;
use time::{Date, OffsetDateTime};

use crate::{config::Tank01Config, normalize};

#[derive(Debug, thiserror::Error)]
pub enum Tank01ClientError {
    #[error("invalid Tank01 configuration: {0}")]
    Configuration(String),
    #[error("failed to build Tank01 HTTP client: {0}")]
    HttpClient(#[source] reqwest::Error),
}

#[derive(Clone)]
pub struct Tank01Client {
    http: reqwest::Client,
    config: Tank01Config,
}

impl std::fmt::Debug for Tank01Client {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Tank01Client")
            .field("http", &self.http)
            .field("config", &self.config)
            .finish()
    }
}

pub(crate) struct HttpObservation {
    requested_at: OffsetDateTime,
    received_at: OffsetDateTime,
    elapsed_ms: u64,
    http_status: Option<u16>,
    headers: BTreeMap<String, String>,
    raw_body: Option<RawBody>,
    transport_error: Option<String>,
}

impl Tank01Client {
    pub fn new(config: Tank01Config) -> Result<Self, Tank01ClientError> {
        if config.api_key.expose().is_empty() {
            return Err(Tank01ClientError::Configuration(
                "API key must not be empty".into(),
            ));
        }
        if config.rapidapi_host.is_empty() {
            return Err(Tank01ClientError::Configuration(
                "RapidAPI host must not be empty".into(),
            ));
        }
        if config.timeout.is_zero() {
            return Err(Tank01ClientError::Configuration(
                "timeout must be greater than zero".into(),
            ));
        }
        let base_url = Url::parse(&config.api_base_url).map_err(|error| {
            Tank01ClientError::Configuration(format!("invalid API base URL: {error}"))
        })?;
        if base_url.host_str().is_none() || !matches!(base_url.scheme(), "http" | "https") {
            return Err(Tank01ClientError::Configuration(
                "API base URL must be an HTTP(S) URL with a host".into(),
            ));
        }

        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(config.timeout)
            .build()
            .map_err(Tank01ClientError::HttpClient)?;
        Ok(Self { http, config })
    }

    pub fn config(&self) -> &Tank01Config {
        &self.config
    }

    pub(crate) async fn request(&self, endpoint: &str, query: &[(&str, &str)]) -> HttpObservation {
        let requested_at = OffsetDateTime::now_utc();
        let started = Instant::now();
        let url = format!(
            "{}/{}",
            self.config.api_base_url.trim_end_matches('/'),
            endpoint.trim_start_matches('/')
        );
        let request = self
            .http
            .get(url)
            .query(query)
            .header("x-rapidapi-key", self.config.api_key.expose())
            .header("x-rapidapi-host", &self.config.rapidapi_host);

        let observation = match request.send().await {
            Ok(response) => {
                let status = response.status().as_u16();
                let headers = safe_headers(response.headers(), self.config.api_key.expose());
                match response.bytes().await {
                    Ok(bytes) => match serde_json::from_slice::<Value>(&bytes) {
                        Ok(mut value) => {
                            redact_json(&mut value, self.config.api_key.expose());
                            HttpObservation {
                                requested_at,
                                received_at: OffsetDateTime::now_utc(),
                                elapsed_ms: started.elapsed().as_millis() as u64,
                                http_status: Some(status),
                                headers,
                                raw_body: Some(RawBody::Json(value)),
                                transport_error: None,
                            }
                        }
                        Err(_) => {
                            let text = String::from_utf8_lossy(&bytes).into_owned();
                            let text =
                                redact_text(&text, self.config.api_key.expose()).into_owned();
                            HttpObservation {
                                requested_at,
                                received_at: OffsetDateTime::now_utc(),
                                elapsed_ms: started.elapsed().as_millis() as u64,
                                http_status: Some(status),
                                headers,
                                raw_body: Some(RawBody::Text(text)),
                                transport_error: None,
                            }
                        }
                    },
                    Err(error) => HttpObservation {
                        requested_at,
                        received_at: OffsetDateTime::now_utc(),
                        elapsed_ms: started.elapsed().as_millis() as u64,
                        http_status: Some(status),
                        headers,
                        raw_body: None,
                        transport_error: Some(error.to_string()),
                    },
                }
            }
            Err(error) => HttpObservation {
                requested_at,
                received_at: OffsetDateTime::now_utc(),
                elapsed_ms: started.elapsed().as_millis() as u64,
                http_status: None,
                headers: BTreeMap::new(),
                raw_body: None,
                transport_error: Some(error.to_string()),
            },
        };
        tracing::debug!(
            endpoint = %endpoint,
            status = ?observation.http_status,
            elapsed_ms = observation.elapsed_ms,
            transport_error = observation.transport_error.as_deref().unwrap_or(""),
            "tank01 request"
        );
        observation
    }

    pub(crate) fn response<T>(
        observation: HttpObservation,
        outcome: ProviderOutcome<T>,
    ) -> ProviderResponse<T> {
        ProviderResponse {
            requested_at: observation.requested_at,
            received_at: observation.received_at,
            elapsed_ms: observation.elapsed_ms,
            http_status: observation.http_status,
            headers: observation.headers,
            raw_body: observation.raw_body,
            outcome,
        }
    }

    pub(crate) fn classify_http(
        observation: &HttpObservation,
    ) -> Result<&Value, LiveProviderError> {
        if let Some(status) = observation
            .http_status
            .filter(|status| !(*status >= 200 && *status < 300))
        {
            let message = match &observation.raw_body {
                Some(RawBody::Text(text)) => text.clone(),
                Some(RawBody::Json(value)) => value.to_string(),
                None => String::new(),
            };
            return Err(LiveProviderError::Http { status, message });
        }
        if let Some(message) = observation.transport_error.clone() {
            return Err(LiveProviderError::Transport { message });
        }
        match &observation.raw_body {
            Some(RawBody::Json(value)) => Ok(value),
            Some(RawBody::Text(text)) => Err(LiveProviderError::Json {
                message: format!("invalid JSON body: {text}"),
            }),
            None => Err(LiveProviderError::Json {
                message: String::from("response body was unavailable"),
            }),
        }
    }
}

impl LiveScoreProvider for Tank01Client {
    async fn scoreboard(&self, date: Date) -> ProviderResponse<LiveScoreboard> {
        let date = normalize::compact_date(date);
        let observation = self
            .request("getNFLScoresOnly", &[("gameDate", &date)])
            .await;
        let value = match Self::classify_http(&observation) {
            Ok(value) => value,
            Err(error) => {
                return Self::response(observation, ProviderOutcome::Error(error));
            }
        };
        match normalize::normalize_scoreboard(value) {
            Ok(scoreboard) => Self::response(observation, ProviderOutcome::Value(scoreboard)),
            Err(error) => Self::response(
                observation,
                ProviderOutcome::Error(error.into_provider_error()),
            ),
        }
    }

    async fn box_score(
        &self,
        game: &LiveGame,
        play_by_play: bool,
    ) -> ProviderResponse<LiveGameSnapshot> {
        let game_id = game.provider_game_id.as_str();
        let query = if play_by_play {
            vec![("gameID", game_id), ("playByPlay", "true")]
        } else {
            vec![("gameID", game_id)]
        };
        let observation = self.request("getNFLBoxScore", &query).await;
        let value = match Self::classify_http(&observation) {
            Ok(value) => value,
            Err(error) => {
                return Self::response(observation, ProviderOutcome::Error(error));
            }
        };
        match normalize::pregame_message_for(value, Some(game_id)) {
            Ok(Some(message)) => {
                return Self::response(observation, ProviderOutcome::Pregame { message });
            }
            Ok(None) => {}
            Err(error) => {
                return Self::response(
                    observation,
                    ProviderOutcome::Error(error.into_provider_error()),
                );
            }
        }
        match normalize::normalize_box_score(value, game, observation.received_at) {
            Ok(snapshot) => Self::response(observation, ProviderOutcome::Value(snapshot)),
            Err(error) => Self::response(
                observation,
                ProviderOutcome::Error(error.into_provider_error()),
            ),
        }
    }
}

fn safe_headers(headers: &HeaderMap, secret: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for (name, value) in headers {
        let name = name.as_str().to_ascii_lowercase();
        if (name == "content-type"
            || name == "date"
            || name == "retry-after"
            || name == "x-rapidapi-request-id"
            || name.starts_with("x-ratelimit-"))
            && let Ok(value) = value.to_str()
        {
            out.insert(name, redact_text(value, secret).into_owned());
        }
    }
    out
}

fn redact_text<'a>(text: &'a str, secret: &str) -> Cow<'a, str> {
    if secret.is_empty() || !text.contains(secret) {
        Cow::Borrowed(text)
    } else {
        Cow::Owned(text.replace(secret, "[redacted]"))
    }
}

fn redact_json(value: &mut Value, secret: &str) {
    if secret.is_empty() {
        return;
    }
    match value {
        Value::String(text) => {
            if text.contains(secret) {
                *text = text.replace(secret, "[redacted]");
            }
        }
        Value::Array(values) => {
            for value in values {
                redact_json(value, secret);
            }
        }
        Value::Object(values) => {
            if values.keys().any(|key| key.contains(secret)) {
                let original = std::mem::take(values);
                *values = original
                    .into_iter()
                    .map(|(key, mut value)| {
                        redact_json(&mut value, secret);
                        (redact_text(&key, secret).into_owned(), value)
                    })
                    .collect();
            } else {
                for value in values.values_mut() {
                    redact_json(value, secret);
                }
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use nfl_data::{
        LiveGame, LiveGamePhase, LiveScoreProvider, ProviderOutcome, RawBody, Season, SeasonType,
        TeamAbbr, Week,
    };
    use time::macros::{date, datetime};
    use utils::Secret;
    use wiremock::matchers::{header, method, path, query_param, query_param_is_missing};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

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

    fn client(server: &MockServer, key: &str) -> Tank01Client {
        Tank01Client::new(
            Tank01Config::new(Secret::new(key))
                .with_base_url(server.uri())
                .with_rapidapi_host("mock.rapidapi.example")
                .with_timeout(Duration::from_secs(5)),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn scoreboard_uses_date_query_and_rapidapi_headers() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/getNFLScoresOnly"))
            .and(query_param("gameDate", "20260910"))
            .and(header("x-rapidapi-key", "secret-key"))
            .and(header("x-rapidapi-host", "mock.rapidapi.example"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "application/json")
                    .set_body_json(serde_json::json!({
                        "20260910_SF@LAR": {
                            "gameID": "20260910_SF@LAR",
                            "home": "LAR",
                            "away": "SF",
                            "gameStatus": "Live - In Progress",
                            "homePts": "0",
                            "awayPts": 0
                        }
                    })),
            )
            .expect(1)
            .mount(&server)
            .await;

        let response = client(&server, "secret-key")
            .scoreboard(date!(2026 - 09 - 10))
            .await;
        assert_eq!(response.http_status, Some(200));
        assert!(matches!(
            &response.outcome,
            ProviderOutcome::Value(value)
                if value.games[0].phase == LiveGamePhase::InProgress
        ));
        assert!(response.headers.contains_key("content-type"));
    }

    #[tokio::test]
    async fn box_score_uses_game_and_optional_play_by_play_queries() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/getNFLBoxScore"))
            .and(query_param("gameID", "20260910_SF@LAR"))
            .and(query_param_is_missing("playByPlay"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "gameID": "20260910_SF@LAR",
                "gameStatusCode": 1,
                "home": "LAR",
                "away": "SF"
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/getNFLBoxScore"))
            .and(query_param("gameID", "20260910_SF@LAR"))
            .and(query_param("playByPlay", "true"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "gameID": "20260910_SF@LAR",
                "gameStatusCode": 1,
                "home": "LAR",
                "away": "SF"
            })))
            .expect(1)
            .mount(&server)
            .await;

        let provider = client(&server, "secret-key");
        assert!(matches!(
            provider.box_score(&game(), false).await.outcome,
            ProviderOutcome::Value(_)
        ));
        assert!(matches!(
            provider.box_score(&game(), true).await.outcome,
            ProviderOutcome::Value(_)
        ));
    }

    #[tokio::test]
    async fn retains_json_and_text_bodies_and_classifies_failures() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/getNFLBoxScore"))
            .and(query_param("gameID", "20260910_SF@LAR"))
            .respond_with(
                ResponseTemplate::new(429)
                    .insert_header("content-type", "text/plain")
                    .set_body_string("rate limited"),
            )
            .expect(1)
            .mount(&server)
            .await;
        let response = client(&server, "secret-key")
            .box_score(&game(), false)
            .await;
        assert!(matches!(
            response.outcome,
            ProviderOutcome::Error(LiveProviderError::Http { status: 429, .. })
        ));
        assert_eq!(
            response.raw_body,
            Some(RawBody::Text("rate limited".into()))
        );

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/getNFLScoresOnly"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{not-json"))
            .mount(&server)
            .await;
        let response = client(&server, "secret-key")
            .scoreboard(date!(2026 - 09 - 10))
            .await;
        assert!(matches!(
            response.outcome,
            ProviderOutcome::Error(LiveProviderError::Json { .. })
        ));
        assert!(matches!(response.raw_body, Some(RawBody::Text(_))));
    }

    #[tokio::test]
    async fn classifies_known_pregame_error_and_never_serializes_key() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/getNFLBoxScore"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("retry-after", "secret-key")
                    .set_body_json(serde_json::json!({
                        "error": "Game hasn't started yet, it will start at 8:35p(ET).",
                        "apiKeyEcho": "secret-key",
                        "secret-key": "key in a JSON object name"
                    })),
            )
            .mount(&server)
            .await;

        let response = client(&server, "secret-key")
            .box_score(&game(), false)
            .await;
        assert!(matches!(response.outcome, ProviderOutcome::Pregame { .. }));
        assert_eq!(
            response.headers.get("retry-after").map(String::as_str),
            Some("[redacted]")
        );
        let serialized = serde_json::to_string(&response).unwrap();
        assert!(!serialized.contains("secret-key"));
        assert!(serialized.contains("[redacted]"));
    }

    #[tokio::test]
    async fn does_not_follow_redirects() {
        let target = MockServer::start().await;
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/getNFLScoresOnly"))
            .respond_with(
                ResponseTemplate::new(302)
                    .insert_header("location", format!("{}/getNFLScoresOnly", target.uri())),
            )
            .expect(1)
            .mount(&server)
            .await;

        let response = client(&server, "secret-key")
            .scoreboard(date!(2026 - 09 - 10))
            .await;
        assert!(matches!(
            response.outcome,
            ProviderOutcome::Error(LiveProviderError::Http { status: 302, .. })
        ));
    }

    #[test]
    fn rejects_invalid_configuration_at_client_construction() {
        let empty_key = Tank01Config::new(Secret::new(""));
        assert!(matches!(
            Tank01Client::new(empty_key),
            Err(Tank01ClientError::Configuration(_))
        ));
        let invalid_url = Tank01Config::new(Secret::new("key")).with_base_url("not a url");
        assert!(matches!(
            Tank01Client::new(invalid_url),
            Err(Tank01ClientError::Configuration(_))
        ));
        let empty_host = Tank01Config::new(Secret::new("key")).with_rapidapi_host("");
        assert!(matches!(
            Tank01Client::new(empty_host),
            Err(Tank01ClientError::Configuration(_))
        ));
        let zero_timeout = Tank01Config::new(Secret::new("key")).with_timeout(Duration::ZERO);
        assert!(matches!(
            Tank01Client::new(zero_timeout),
            Err(Tank01ClientError::Configuration(_))
        ));
    }
}
