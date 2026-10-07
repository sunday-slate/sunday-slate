use std::collections::HashMap;

use nfl_model::{
    EspnPlayerId, InjuryDesignation, InjuryEntry, InjuryProvider, InjuryReport, LiveProviderError,
    ProviderOutcome, ProviderResponse, TeamAbbr,
};
use serde::Deserialize;
use serde_json::Value;
use time::Date;

use crate::client::Tank01Client;

const INJURIES_ENDPOINT: &str = "getNFLInjuriesByDate";

const COMPACT_DATE: &[time::format_description::BorrowedFormatItem<'static>] =
    time::macros::format_description!("[year][month padding:zero][day padding:zero]");

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawBody {
    inj_list: Option<Vec<RawRow>>,
    inj_date: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawRow {
    #[serde(rename = "playerID")]
    player_id: Option<String>,
    long_name: String,
    team: Option<String>,
    pos: Option<String>,
    designation: String,
}

impl Tank01Client {
    fn parse(value: &Value) -> Result<InjuryReport, LiveProviderError> {
        let body = value
            .get("body")
            .filter(|body| body.is_object())
            .ok_or_else(|| LiveProviderError::Normalization {
                message: "injuries body missing".into(),
            })?;
        let raw = RawBody::deserialize(body).map_err(|error| LiveProviderError::Normalization {
            message: format!("injuries body malformed: {error}"),
        })?;
        let inj_list = raw
            .inj_list
            .ok_or_else(|| LiveProviderError::Normalization {
                message: "injList missing".into(),
            })?;

        let mut entries: Vec<InjuryEntry> = Vec::new();
        let mut slots: HashMap<String, usize> = HashMap::new();
        for raw in &inj_list {
            let Some(entry) = map_row(raw) else {
                continue;
            };
            match entry.espn_id.as_ref() {
                Some(EspnPlayerId(key)) => match slots.get(key).copied() {
                    Some(slot) => entries[slot] = entry,
                    None => {
                        slots.insert(key.clone(), entries.len());
                        entries.push(entry);
                    }
                },
                None => entries.push(entry),
            }
        }

        Ok(InjuryReport {
            report_date: raw
                .inj_date
                .as_deref()
                .and_then(|raw| Date::parse(raw, COMPACT_DATE).ok()),
            entries,
        })
    }
}

fn map_row(raw: &RawRow) -> Option<InjuryEntry> {
    let designation = match raw.designation.as_str() {
        "Q" => InjuryDesignation::Questionable,
        "D" => InjuryDesignation::Doubtful,
        "O" => InjuryDesignation::Out,
        "IR" => InjuryDesignation::InjuredReserve,
        other => {
            tracing::warn!(
                player = %raw.long_name,
                code = %other,
                "unknown injury designation; dropping row"
            );
            return None;
        }
    };
    let team = raw
        .team
        .as_deref()
        .map(str::trim)
        .filter(|team| !team.is_empty())
        .map(|team| TeamAbbr(team.into()));
    let player_id = raw
        .player_id
        .as_deref()
        .filter(|id| !id.is_empty())
        .map(|id| EspnPlayerId(id.into()));
    Some(InjuryEntry {
        espn_id: player_id,
        name: raw.long_name.clone(),
        team,
        position: raw.pos.clone(),
        designation,
    })
}

impl InjuryProvider for Tank01Client {
    async fn current_injuries(&self) -> ProviderResponse<InjuryReport> {
        let observation = self.request(INJURIES_ENDPOINT, &[]).await;
        let parsed = Tank01Client::classify_http(&observation).and_then(Tank01Client::parse);
        match parsed {
            Ok(report) => Tank01Client::response(observation, ProviderOutcome::Value(report)),
            Err(LiveProviderError::Normalization { .. }) => Tank01Client::response(
                observation,
                ProviderOutcome::Pregame {
                    message: "injuries body unavailable".into(),
                },
            ),
            Err(error) => Tank01Client::response(observation, ProviderOutcome::Error(error)),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use nfl_model::LiveProviderError;
    use utils::Secret;

    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;
    use crate::config::Tank01Config;

    fn client(server: &MockServer) -> Tank01Client {
        Tank01Client::new(
            Tank01Config::new(Secret::new("secret-key"))
                .with_base_url(server.uri())
                .with_rapidapi_host("mock.rapidapi.example")
                .with_timeout(Duration::from_secs(5)),
        )
        .unwrap()
    }

    fn bodies() -> Vec<Value> {
        include_str!("../tests/fixtures/tank01_injury_bodies.ndjson")
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| {
                let envelope: Value = serde_json::from_str(line).unwrap();
                envelope["body"].clone()
            })
            .collect()
    }

    #[tokio::test]
    async fn parses_entries_drops_unknown_and_keeps_later_duplicate() {
        let server = MockServer::start().await;
        let body = bodies()[0].clone();
        Mock::given(method("GET"))
            .and(path("/getNFLInjuriesByDate"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "application/json")
                    .set_body_json(body),
            )
            .expect(1)
            .mount(&server)
            .await;

        let response = client(&server).current_injuries().await;
        assert_eq!(response.http_status, Some(200));
        let report = match &response.outcome {
            ProviderOutcome::Value(report) => report,
            other => panic!("expected parsed injuries, got {other:?}"),
        };
        assert_eq!(report.entries.len(), 4);
        assert_eq!(
            report.report_date,
            Some(time::macros::date!(2026 - 09 - 22))
        );

        let kupp = report
            .entries
            .iter()
            .find(|entry| entry.name == "Cooper Kupp")
            .unwrap();
        assert_eq!(kupp.designation, InjuryDesignation::Doubtful);
        assert_eq!(kupp.espn_id, Some(EspnPlayerId("18538".into())));
        assert_eq!(kupp.team, Some(TeamAbbr("SEA".into())));
        assert_eq!(kupp.position.as_deref(), Some("WR"));

        let hennessy = report
            .entries
            .iter()
            .find(|entry| entry.name == "Matt Hennessy")
            .unwrap();
        assert_eq!(hennessy.designation, InjuryDesignation::InjuredReserve);
        assert!(hennessy.espn_id.is_some());

        let allen = report
            .entries
            .iter()
            .find(|entry| entry.name == "Josh Allen")
            .unwrap();
        assert_eq!(allen.designation, InjuryDesignation::Questionable);

        let carter = report
            .entries
            .iter()
            .find(|entry| entry.name == "Jalen Carter")
            .unwrap();
        assert_eq!(carter.designation, InjuryDesignation::Out);

        assert!(
            !report
                .entries
                .iter()
                .any(|entry| entry.name == "Marlon Humphrey")
        );
    }

    #[tokio::test]
    async fn treats_body_without_inj_list_as_pregame() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/getNFLInjuriesByDate"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "statusCode": 200,
                "body": {}
            })))
            .expect(1)
            .mount(&server)
            .await;

        let response = client(&server).current_injuries().await;
        assert!(
            matches!(&response.outcome, ProviderOutcome::Pregame { .. }),
            "expected pregame, got {:?}",
            response.outcome
        );
        let serialized = serde_json::to_string(&response).unwrap();
        assert!(!serialized.contains("secret-key"));
    }

    #[tokio::test]
    async fn surfaces_http_failures_as_provider_errors() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/getNFLInjuriesByDate"))
            .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
            .expect(1)
            .mount(&server)
            .await;

        let response = client(&server).current_injuries().await;
        assert!(matches!(
            &response.outcome,
            ProviderOutcome::Error(LiveProviderError::Http { status: 500, .. })
        ));
    }
}
