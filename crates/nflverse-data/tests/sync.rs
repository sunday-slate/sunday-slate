use nflverse_data::{
    DatasetStatus, NflverseData, NflverseDataConfig, Season, SeasonType, TeamAbbr, Week,
};
use time::macros::datetime;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const TAGS: [&str; 6] = [
    "schedules",
    "players",
    "rosters",
    "weekly_rosters",
    "stats_player",
    "pbp",
];

#[derive(Clone)]
struct FakeAsset {
    tag: &'static str,
    name: &'static str,
    updated_at: &'static str,
    body: Vec<u8>,
}

fn fixtures() -> Vec<FakeAsset> {
    vec![
        FakeAsset {
            tag: "schedules",
            name: "games.csv",
            updated_at: "2026-06-30T10:36:16Z",
            body: include_bytes!("fixtures/games.csv").to_vec(),
        },
        FakeAsset {
            tag: "players",
            name: "players.csv",
            updated_at: "2026-07-01T11:22:19Z",
            body: include_bytes!("fixtures/players.csv").to_vec(),
        },
        FakeAsset {
            tag: "rosters",
            name: "roster_2025.csv",
            updated_at: "2026-06-20T00:00:00Z",
            body: include_bytes!("fixtures/roster_2025.csv").to_vec(),
        },
        FakeAsset {
            tag: "weekly_rosters",
            name: "roster_weekly_2025.csv",
            updated_at: "2026-08-01T00:00:00Z",
            body: include_bytes!("fixtures/roster_weekly_2025.csv").to_vec(),
        },
        FakeAsset {
            tag: "stats_player",
            name: "stats_player_week_2025.csv",
            updated_at: "2026-01-15T00:00:00Z",
            body: include_bytes!("fixtures/stats_player_week_2025.csv").to_vec(),
        },
        FakeAsset {
            tag: "pbp",
            name: "play_by_play_2025.csv",
            updated_at: "2026-01-15T00:00:00Z",
            body: include_bytes!("fixtures/play_by_play_2025.csv").to_vec(),
        },
    ]
}

/// Serves the release API and asset downloads for the given assets. Serving
/// plain .csv (never .csv.gz) also exercises the gz→csv fallback in selection.
async fn mock_nflverse(assets: &[FakeAsset]) -> MockServer {
    let server = MockServer::start().await;
    for tag in TAGS {
        let release_assets: Vec<_> = assets
            .iter()
            .filter(|a| a.tag == tag)
            .map(|a| {
                serde_json::json!({
                    "name": a.name,
                    "updated_at": a.updated_at,
                    "browser_download_url":
                        format!("{}/download/{}/{}", server.uri(), tag, a.name),
                })
            })
            .collect();
        Mock::given(method("GET"))
            .and(path(format!(
                "/repos/nflverse/nflverse-data/releases/tags/{tag}"
            )))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "assets": release_assets })),
            )
            .mount(&server)
            .await;
    }
    for a in assets {
        Mock::given(method("GET"))
            .and(path(format!("/download/{}/{}", a.tag, a.name)))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(a.body.clone()))
            .mount(&server)
            .await;
    }
    server
}

fn config(server: &MockServer, dir: &tempfile::TempDir) -> NflverseDataConfig {
    NflverseDataConfig {
        database_url: format!("sqlite://{}/nflverse-data.db", dir.path().display()),
        earliest_season: 2025,
        github_token: None,
        github_api_base: server.uri(),
    }
}

fn assert_all_updated(report: &nflverse_data::SyncReport) {
    for d in &report.datasets {
        assert!(
            matches!(d.status, DatasetStatus::Updated { .. }),
            "expected {} updated, got {:?}",
            d.dataset.name(),
            d.status
        );
    }
}

#[tokio::test]
async fn fresh_sync_populates_every_dataset() {
    let server = mock_nflverse(&fixtures()).await;
    let dir = tempfile::tempdir().unwrap();
    let nfl = NflverseData::connect(config(&server, &dir)).await.unwrap();

    let report = nfl.sync().await.unwrap();
    assert_all_updated(&report);

    // Games: 1999 and 2024 rows filtered out; kickoff in UTC; SB mapped to Post.
    let games = nfl.games(Season(2025)).await.unwrap();
    assert_eq!(games.len(), 3);
    let sf_la = games
        .iter()
        .find(|g| g.gsis_game_id == "2025_05_SF_LA")
        .unwrap();
    assert_eq!(sf_la.kickoff, Some(datetime!(2025-10-03 00:15 UTC)));
    let sb = games
        .iter()
        .find(|g| g.gsis_game_id == "2025_22_SEA_NE")
        .unwrap();
    assert_eq!(sb.season_type, SeasonType::Post);

    // Players.
    let players = nfl.players().await.unwrap();
    assert_eq!(players.len(), 2);
    assert_eq!(players[1].full_name, "Jordan Love");

    // Roster, including the row with no gsis_id.
    let roster = nfl
        .roster(Season(2025), &TeamAbbr("GB".into()))
        .await
        .unwrap();
    assert_eq!(roster.len(), 2);
    assert_eq!(roster[0].full_name, "Dante Barnett");
    assert_eq!(roster[0].gsis_id, None);

    // Weekly roster: Thielen syncs onto the team he was on that week, so the
    // week-13 MIN row and the week-14 PIT row are both retrievable.
    let min_13 = nfl
        .weekly_roster(Season(2025), Week(13), &TeamAbbr("MIN".into()))
        .await
        .unwrap();
    assert_eq!(min_13.len(), 1);
    assert_eq!(min_13[0].full_name, "Adam Thielen");
    let pit_14 = nfl
        .weekly_roster(Season(2025), Week(14), &TeamAbbr("PIT".into()))
        .await
        .unwrap();
    assert_eq!(pit_14.len(), 1);
    assert_eq!(pit_14[0].full_name, "Adam Thielen");
    assert!(
        nfl.weekly_roster(Season(2025), Week(13), &TeamAbbr("PIT".into()))
            .await
            .unwrap()
            .is_empty(),
        "not yet traded in week 13"
    );

    // Player week stats.
    let stats = nfl.player_week_stats(Season(2025), Week(1)).await.unwrap();
    assert_eq!(stats.len(), 2);
    let rodgers = &stats[0];
    assert_eq!(rodgers.gsis_id, "00-0023459");
    assert_eq!(rodgers.passing_yards, 244);
    assert_eq!(rodgers.fantasy_points, 25.66);
    let prater = &stats[1];
    assert_eq!(prater.gsis_id, "00-0023853");
    assert_eq!(prater.team, TeamAbbr("BUF".into()));
    assert_eq!(prater.fantasy_points, 0.0);

    // Team week stats are pbp-derived: the one game yields both teams' D/ST
    // rows (ordered by team). points_allowed is the OPPONENT's offensive points
    // computed at ingest -- ARI allowed NO's 6, NO allowed ARI's 7+3=10.
    let team = nfl.team_week_stats(Season(2025), Week(1)).await.unwrap();
    assert_eq!(team.len(), 2);
    assert_eq!(team[0].team, TeamAbbr("ARI".into()));
    assert_eq!(team[0].opponent, TeamAbbr("NO".into()));
    assert_eq!(team[0].sacks, 1);
    assert_eq!(team[0].points_allowed, 6);
    assert_eq!(team[1].team, TeamAbbr("NO".into()));
    assert_eq!(team[1].points_allowed, 10);
}

#[tokio::test]
async fn second_sync_with_same_updated_at_is_unchanged() {
    let server = mock_nflverse(&fixtures()).await;
    let dir = tempfile::tempdir().unwrap();
    let nfl = NflverseData::connect(config(&server, &dir)).await.unwrap();

    nfl.sync().await.unwrap();
    let report = nfl.sync().await.unwrap();

    for d in &report.datasets {
        assert!(
            matches!(d.status, DatasetStatus::Unchanged),
            "expected {} unchanged, got {:?}",
            d.dataset.name(),
            d.status
        );
    }
}

#[tokio::test]
async fn bumped_asset_replaces_only_its_scope() {
    let server = mock_nflverse(&fixtures()).await;
    let dir = tempfile::tempdir().unwrap();
    let nfl = NflverseData::connect(config(&server, &dir)).await.unwrap();
    nfl.sync().await.unwrap();
    drop(server);

    // Same fixtures except player stats: new updated_at, Rodgers now at 300 yards.
    let mut assets = fixtures();
    let stats = assets.iter_mut().find(|a| a.tag == "stats_player").unwrap();
    stats.updated_at = "2026-01-16T00:00:00Z";
    stats.body = String::from_utf8(stats.body.clone())
        .unwrap()
        .replace(",244,", ",300,")
        .into_bytes();
    let server = mock_nflverse(&assets).await;
    let nfl = NflverseData::connect(NflverseDataConfig {
        github_api_base: server.uri(),
        ..config(&server, &dir)
    })
    .await
    .unwrap();

    let report = nfl.sync().await.unwrap();
    for d in &report.datasets {
        match d.dataset.name() {
            "player_week_stats" => {
                assert!(matches!(
                    d.status,
                    DatasetStatus::Updated { assets: 1, rows: 2 }
                ))
            }
            name => assert!(
                matches!(d.status, DatasetStatus::Unchanged),
                "expected {name} unchanged, got {:?}",
                d.status
            ),
        }
    }

    let stats = nfl.player_week_stats(Season(2025), Week(1)).await.unwrap();
    assert_eq!(stats.len(), 2); // replaced, not appended
    assert_eq!(stats[0].passing_yards, 300);
}

#[tokio::test]
async fn failing_dataset_is_isolated_and_keeps_previous_data() {
    let server = mock_nflverse(&fixtures()).await;
    let dir = tempfile::tempdir().unwrap();
    let nfl = NflverseData::connect(config(&server, &dir)).await.unwrap();
    nfl.sync().await.unwrap();
    drop(server);

    // Bump the pbp asset but serve a malformed body.
    let mut assets = fixtures();
    let team = assets.iter_mut().find(|a| a.tag == "pbp").unwrap();
    team.updated_at = "2026-01-16T00:00:00Z";
    team.body = b"this,is,not,the,schema\n1,2,3,4,5\n".to_vec();
    let server = mock_nflverse(&assets).await;
    let nfl = NflverseData::connect(NflverseDataConfig {
        github_api_base: server.uri(),
        ..config(&server, &dir)
    })
    .await
    .unwrap();

    let report = nfl.sync().await.unwrap();
    assert!(!report.all_ok());
    for d in &report.datasets {
        match d.dataset.name() {
            "team_week_stats" => assert!(matches!(d.status, DatasetStatus::Failed(_))),
            name => assert!(
                matches!(d.status, DatasetStatus::Unchanged),
                "expected {name} unchanged, got {:?}",
                d.status
            ),
        }
    }

    // Parse failed before any write was attempted: previous rows are intact.
    let team = nfl.team_week_stats(Season(2025), Week(1)).await.unwrap();
    assert_eq!(team.len(), 2);
    assert_eq!(team[0].team, TeamAbbr("ARI".into()));
    assert_eq!(team[0].points_allowed, 6);
}

#[tokio::test]
async fn one_bad_season_does_not_block_other_seasons_in_the_same_dataset() {
    // Two seasons in the window for a per-season dataset (rosters): 2024's
    // asset is malformed and fails to parse, 2025's asset is valid. Before
    // the fix, the ascending-season loop tried 2024 first, hit `?`, and
    // aborted before ever attempting 2025 — so 2025's perfectly good data
    // never got stored. After the fix, both assets are attempted, so 2025's
    // roster is stored even though the dataset is reported Failed overall.
    let mut assets = fixtures();
    assets.push(FakeAsset {
        tag: "rosters",
        name: "roster_2024.csv",
        updated_at: "2026-06-19T00:00:00Z",
        body: b"this,is,not,the,schema\n1,2,3,4,5\n".to_vec(),
    });
    let server = mock_nflverse(&assets).await;
    let dir = tempfile::tempdir().unwrap();
    let nfl = NflverseData::connect(NflverseDataConfig {
        earliest_season: 2024,
        ..config(&server, &dir)
    })
    .await
    .unwrap();

    let report = nfl.sync().await.unwrap();
    assert!(!report.all_ok());
    for d in &report.datasets {
        match d.dataset.name() {
            "rosters" => assert!(
                matches!(d.status, DatasetStatus::Failed(_)),
                "expected rosters Failed, got {:?}",
                d.status
            ),
            name => assert!(
                matches!(d.status, DatasetStatus::Updated { .. }),
                "expected {name} updated, got {:?}",
                d.status
            ),
        }
    }

    // The good 2025 season was still attempted and stored, despite the
    // dataset-level status being Failed because of the bad 2024 asset.
    let roster = nfl
        .roster(Season(2025), &TeamAbbr("GB".into()))
        .await
        .unwrap();
    assert_eq!(roster.len(), 2);
    assert_eq!(roster[0].full_name, "Dante Barnett");
}
