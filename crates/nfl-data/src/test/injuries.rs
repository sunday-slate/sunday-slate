use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use nfl_model::{DfsPositionSlotType, NflPlayerId};
use tank01_data::Tank01Config;
use utils::Secret;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

use crate::store::player_link::{self, PlayerLink};
use crate::{
    DfsPosition, Game, InjuryDesignation, NflData, NflDataConfig, Player, Season, SeasonWeek,
    Snapshot, TeamAbbr, Week, salaries, slates,
};

const SEASON: Season = Season(2026);
const WEEK: u8 = 5;
const MACK: &str = "00-MACK";

struct Tank01Mock {
    server: MockServer,
    requests: Arc<AtomicUsize>,
    response: Arc<Mutex<(u16, String)>>,
}

impl Tank01Mock {
    async fn start() -> Self {
        let server = MockServer::start().await;
        let requests = Arc::new(AtomicUsize::new(0));
        let response = Arc::new(Mutex::new((200, injury_body("Q"))));
        let observed = Arc::clone(&requests);
        let reply = Arc::clone(&response);
        Mock::given(method("GET"))
            .and(path("/getNFLInjuriesByDate"))
            .respond_with(move |_request: &Request| {
                observed.fetch_add(1, Ordering::SeqCst);
                let (status, body) = reply
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .clone();
                ResponseTemplate::new(status)
                    .insert_header("content-type", "application/json")
                    .set_body_string(body)
            })
            .mount(&server)
            .await;
        Self {
            server,
            requests,
            response,
        }
    }

    fn set(&self, status: u16, body: impl Into<String>) {
        *self
            .response
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = (status, body.into());
    }

    fn requests(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }

    fn config(&self) -> NflDataConfig {
        NflDataConfig {
            nflverse_database_url: "sqlite::memory:".into(),
            fanduel_database_url: "sqlite::memory:".into(),
            database_url: "sqlite::memory:".into(),
            season: SEASON,
            tank01: Some(
                Tank01Config::new(Secret::new("tank01-key"))
                    .with_base_url(self.server.uri())
                    .with_rapidapi_host("mock.rapidapi.example"),
            ),
            ..NflDataConfig::default()
        }
    }
}

fn injury_body(designation: &str) -> String {
    format!(
        r#"{{"statusCode":200,"body":{{"injList":[{{"team":"KC","designation":"{designation}","pos":"RB","playerID":"ESPN-MACK","longName":"Mack Runner"}}],"injDate":"20260922"}}}}"#
    )
}

async fn seed(nfl: &NflData) {
    let season_week = SeasonWeek {
        season: SEASON,
        week: Week(WEEK),
    };
    nfl.seed_for_test(
        &[Player {
            gsis_id: MACK.into(),
            espn_id: Some("ESPN-MACK".into()),
            full_name: "Mack Runner".into(),
            first_name: None,
            last_name: None,
            position: Some("RB".into()),
            latest_team: Some(TeamAbbr::KC),
            headshot_url: None,
        }],
        &[Game {
            game_id: "2026_05_BUF_KC".into(),
            season_week,
            kickoff: Some(time::macros::datetime!(2026 - 10 - 04 17:00 UTC)),
            home_team: TeamAbbr::BUF,
            away_team: TeamAbbr::KC,
            home_score: None,
            away_score: None,
        }],
    )
    .await
    .expect("seed nflverse");
    let csv = "\
Id,Position,First Name,Nickname,Last Name,FPPG,Played,Salary,Game,Team,Opponent,Injury Indicator,Injury Details,Tier,,,Roster Position
9001-1,RB,Mack,Mack Runner,Runner,,0,9000,KC@BUF,KC,BUF,,,,,,RB/FLEX
";
    let rows = salaries::parse_fanduel_csv(&nfl.fanduel, season_week, csv.as_bytes())
        .expect("parse salaries");
    salaries::save_fanduel_salaries(&nfl.fanduel, &rows)
        .await
        .expect("save salaries");
    player_link::save_links(
        &nfl.store,
        &[PlayerLink {
            fd_player_id: 1,
            gsis_player_id: MACK.into(),
            espn_player_id: Some("ESPN-MACK".into()),
        }],
    )
    .await
    .expect("save link");
    slates::populate_at(
        &nfl.nflverse,
        &nfl.store,
        SEASON,
        time::macros::datetime!(2026 - 09 - 01 12:00 UTC),
    )
    .await
    .expect("populate slates");
}

fn slate_id() -> slates::SlateId {
    slates::SlateId(
        i64::from(
            SeasonWeek {
                season: SEASON,
                week: Week(WEEK),
            }
            .encode(),
        ) * 10
            + 1,
    )
}

fn injury(snapshot: &Snapshot) -> Option<InjuryDesignation> {
    snapshot
        .player(&slate_id(), &NflPlayerId(MACK.into()))
        .expect("mack is built")
        .injury
}

async fn wait_for_injury(nfl: &NflData, expected: Option<InjuryDesignation>) {
    for _ in 0..500 {
        if injury(&nfl.snapshot()) == expected {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!(
        "injury never became {expected:?}; published {:?}",
        injury(&nfl.snapshot())
    );
}

// Reenablement avoids waiting five real minutes between mock responses.
async fn poll_again(nfl: &Arc<NflData>, mock: &Tank01Mock, expected_requests: usize) {
    nfl.stop_background_tasks();
    tokio::time::sleep(Duration::from_millis(50)).await;
    nfl.start_background_tasks();
    for _ in 0..500 {
        if mock.requests() >= expected_requests {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!(
        "expected {expected_requests} injury requests, saw {}",
        mock.requests()
    );
}

#[tokio::test]
async fn configured_source_publishes_and_preserves_injury_input() {
    let mock = Tank01Mock::start().await;
    let nfl = Arc::new(NflData::connect(mock.config()).await.expect("connect"));
    assert_eq!(mock.requests(), 0, "connect makes no injury request");
    seed(&nfl).await;
    nfl.rebuild_snapshot().await.expect("initial rebuild");
    assert_eq!(injury(&nfl.snapshot()), None);

    nfl.start_background_tasks();
    wait_for_injury(&nfl, Some(InjuryDesignation::Questionable)).await;
    assert_eq!(mock.requests(), 1);

    nfl.rebuild_snapshot().await.expect("rebuild");
    assert_eq!(
        injury(&nfl.snapshot()),
        Some(InjuryDesignation::Questionable)
    );

    mock.set(500, "boom");
    poll_again(&nfl, &mock, 2).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    nfl.rebuild_snapshot()
        .await
        .expect("rebuild after the failed poll");
    assert_eq!(
        injury(&nfl.snapshot()),
        Some(InjuryDesignation::Questionable),
        "a failed poll leaves the accepted report as the retained input"
    );

    let old = nfl.snapshot();
    mock.set(
        200,
        r#"{"statusCode":200,"body":{"injList":[],"injDate":"20260923"}}"#,
    );
    poll_again(&nfl, &mock, 3).await;
    wait_for_injury(&nfl, None).await;
    assert_eq!(
        injury(&old),
        Some(InjuryDesignation::Questionable),
        "published snapshots are immutable"
    );
    let cleared = nfl.snapshot();
    assert_eq!(
        cleared
            .salaried_players_for_position(
                &slate_id(),
                &DfsPositionSlotType::Position(DfsPosition::Rb)
            )
            .first()
            .expect("mack fills the bucket")
            .injury,
        None
    );
}

#[tokio::test]
async fn handles_without_a_tank01_config_never_poll() {
    let nfl = Arc::new(
        NflData::connect(NflDataConfig {
            nflverse_database_url: "sqlite::memory:".into(),
            fanduel_database_url: "sqlite::memory:".into(),
            database_url: "sqlite::memory:".into(),
            season: SEASON,
            ..NflDataConfig::default()
        })
        .await
        .expect("connect"),
    );
    assert!(nfl.injuries.poller.is_none(), "the source is disabled");
    seed(&nfl).await;
    nfl.rebuild_snapshot().await.expect("rebuild");
    nfl.start_background_tasks();
    tokio::task::yield_now().await;
    assert!(nfl.injury_report().is_none(), "no source means no input");
    assert_eq!(injury(&nfl.snapshot()), None);
}
