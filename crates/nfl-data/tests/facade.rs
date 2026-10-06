use nfl_data::{
    Dataset, DatasetStatus, Game, NflData, NflDataConfig, Player, Season, SeasonType, TeamAbbr,
    Week, WeeklyRosterEntry,
};
use utils::Secret;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{header, method, path},
};

fn player() -> Player {
    Player {
        gsis_id: "00-P".into(),
        espn_id: Some("42".into()),
        full_name: "Player Table".into(),
        first_name: None,
        last_name: None,
        position: Some("RB".into()),
        latest_team: Some(TeamAbbr("KC".into())),
        status: None,
        birth_date: None,
        headshot_url: Some("https://example.test/player.png".into()),
    }
}

fn game() -> Game {
    Game {
        gsis_game_id: "2025_01_BUF_KC".into(),
        season: Season(2025),
        week: Week(1),
        season_type: SeasonType::Reg,
        kickoff: None,
        home_team: TeamAbbr("KC".into()),
        away_team: TeamAbbr("BUF".into()),
        home_score: None,
        away_score: None,
    }
}

#[tokio::test]
async fn empty_provider_reads_preserve_missing_result_behavior() {
    let nfl = NflData::in_memory().await.unwrap();
    assert!(nfl.games(Season(2025)).await.unwrap().is_empty());
    assert!(nfl.players().await.unwrap().is_empty());
    assert!(nfl.players_by_gsis(&["missing"]).await.unwrap().is_empty());
    assert!(
        nfl.weekly_roster_entries_by_gsis(&["missing"])
            .await
            .unwrap()
            .is_empty()
    );
    assert!(nfl.identify(&["missing"]).await.unwrap().is_empty());
    let team = TeamAbbr("KC".into());
    assert!(nfl.roster(Season(2025), &team).await.unwrap().is_empty());
    assert!(
        nfl.weekly_roster(Season(2025), Week(1), &team)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        nfl.player_week_stats(Season(2025), Week(1))
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        nfl.player_season_totals(Season(2025), Week(2))
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        nfl.team_week_stats(Season(2025), Week(1))
            .await
            .unwrap()
            .is_empty()
    );
    let freshness = nfl.freshness().await.unwrap();
    assert_eq!(freshness.len(), 6);
    assert!(
        freshness
            .iter()
            .all(|row| row.assets == 0 && row.last_synced_at.is_none())
    );
}

#[tokio::test]
async fn seeded_records_survive_reopening_and_identity_prefers_players() {
    let dir = tempfile::tempdir().unwrap();
    let config = NflDataConfig {
        database_url: format!("sqlite://{}/cache.db", dir.path().display()),
        ..Default::default()
    };
    let nfl = NflData::connect(config.clone()).await.unwrap();
    nfl.seed_for_test(&[player()], &[game()]).await.unwrap();
    let roster = |id: &str| WeeklyRosterEntry {
        season: Season(2025),
        week: Week(1),
        team: TeamAbbr("BUF".into()),
        gsis_id: Some(id.into()),
        espn_id: None,
        full_name: "Roster Name".into(),
        last_name: None,
        position: Some("WR".into()),
        status: "ACT".into(),
    };
    nfl.seed_weekly_roster_for_test(&[roster("00-P"), roster("00-R")])
        .await
        .unwrap();
    drop(nfl);
    let nfl = NflData::connect(config).await.unwrap();
    assert_eq!(nfl.games(Season(2025)).await.unwrap(), [game()]);
    assert_eq!(nfl.players().await.unwrap(), [player()]);
    let players = nfl
        .players_by_gsis(&["00-P", "00-P", "missing"])
        .await
        .unwrap();
    assert_eq!(players.len(), 1);
    assert_eq!(players["00-P"], player());
    assert_eq!(
        nfl.weekly_roster(Season(2025), Week(1), &TeamAbbr("BUF".into()))
            .await
            .unwrap()
            .len(),
        2
    );
    let identities = nfl.identify(&["00-P", "00-R", "missing"]).await.unwrap();
    assert_eq!(identities.len(), 2);
    assert_eq!(identities["00-P"].name, "Player Table");
    assert_eq!(identities["00-P"].headshot_url, player().headshot_url);
    assert_eq!(identities["00-R"].name, "Roster Name");
    assert_eq!(identities["00-R"].team, Some(TeamAbbr("BUF".into())));
    assert!(identities["00-R"].headshot_url.is_none());
    assert!(
        nfl.freshness()
            .await
            .unwrap()
            .iter()
            .all(|row| row.assets == 0)
    );
}

#[tokio::test]
async fn sync_forwards_upstream_configuration_and_preserves_partial_failure_reports() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let tags = [
        "schedules",
        "players",
        "rosters",
        "weekly_rosters",
        "stats_player",
        "pbp",
    ];
    for tag in tags {
        let response = if tag == "schedules" {
            ResponseTemplate::new(200).set_body_string(format!(
                r#"{{"assets":[{{"name":"games.csv","browser_download_url":"{}/games.csv","updated_at":"2026-01-01T00:00:00Z"}}]}}"#,
                server.uri()
            )).insert_header("content-type", "application/json")
        } else {
            ResponseTemplate::new(503)
        };
        Mock::given(method("GET"))
            .and(path(format!(
                "/repos/nflverse/nflverse-data/releases/tags/{tag}"
            )))
            .and(header("authorization", "Bearer sentinel"))
            .respond_with(response)
            .expect(2)
            .mount(&server)
            .await;
    }
    Mock::given(method("GET")).and(path("/games.csv"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            "game_id,season,week,game_type,gameday,gametime,home_team,away_team,home_score,away_score\n2025_01_BUF_KC,2025,1,REG,2025-09-07,13:00,KC,BUF,,\n2024_01_BUF_KC,2024,1,REG,2024-09-08,13:00,KC,BUF,,\n"
        )).expect(1).mount(&server).await;
    let nfl = NflData::connect(NflDataConfig {
        database_url: format!("sqlite://{}/cache.db", dir.path().display()),
        earliest_season: 2025,
        github_token: Some(Secret::new("sentinel")),
        github_api_base: server.uri(),
    })
    .await
    .unwrap();
    let report = nfl.sync().await.unwrap();
    assert!(!report.all_ok());
    assert_eq!(report.datasets.len(), 6);
    assert!(matches!(
        report.datasets[0].status,
        DatasetStatus::Updated { assets: 1, rows: 1 }
    ));
    assert!(
        report.datasets[1..]
            .iter()
            .all(|row| matches!(row.status, DatasetStatus::Failed(_)))
    );
    assert!(report.to_string().contains("FAILED"));
    assert_eq!(nfl.games(Season(2025)).await.unwrap().len(), 1);
    assert!(nfl.games(Season(2024)).await.unwrap().is_empty());
    let freshness = nfl.freshness().await.unwrap();
    assert_eq!(freshness[0].dataset, Dataset::Schedules);
    assert_eq!(freshness[0].assets, 1);
    assert!(freshness[0].last_synced_at.is_some());
    use std::time::Duration;
    use utils::background::RunnerState;
    assert!(matches!(
        nfl.refresh_status(),
        RunnerState::Idle { last: None }
    ));
    assert!(!nfl.stop_scheduler());
    assert!(!nfl.start_scheduler(Duration::ZERO));
    assert!(nfl.start_scheduler(Duration::from_secs(60)));
    assert!(!nfl.start_scheduler(Duration::from_secs(60)));
    assert!(nfl.stop_scheduler());
    assert!(nfl.request_refresh().await.unwrap());
    let last = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let RunnerState::Idle { last: Some(last) } = nfl.refresh_status() {
                break last;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let report = last.outcome.unwrap_err();
    assert!(report.contains("FAILED"));
    assert!(report.contains("unchanged"));
}
