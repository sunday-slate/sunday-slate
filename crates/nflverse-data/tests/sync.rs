use nflverse_data::{
    AssetUpdate, Dataset, DatasetReport, DatasetStatus, NflverseData, NflverseDataConfig, Season,
    SeasonWeek, SyncEvent, TeamAbbr, Week,
};
use time::macros::datetime;

/// A validated identity for the 2025 fixtures below.
fn week(season: u16, week: u8) -> SeasonWeek {
    SeasonWeek::new(Season(season), Week(week)).expect("valid fixture week")
}

use tokio::sync::mpsc;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const TAGS: [&str; 5] = [
    "schedules",
    "players",
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
        database_url: format!("sqlite://{}/nflverse-cache.db", dir.path().display()),
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

/// The report row for one dataset. Datasets appear in `Dataset::ALL` order,
/// but lookup by value keeps the assertions independent of that order.
fn dataset_report(report: &nflverse_data::SyncReport, dataset: Dataset) -> &DatasetReport {
    report
        .datasets
        .iter()
        .find(|d| d.dataset == dataset)
        .unwrap()
}

/// Connects with the typed completion-event channel, returning the receiver.
async fn connect_with_events(
    config: NflverseDataConfig,
) -> (NflverseData, mpsc::UnboundedReceiver<SyncEvent>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let nfl = NflverseData::connect_with_sync_events(config, Some(tx))
        .await
        .unwrap();
    (nfl, rx)
}

fn assert_updates(event: &SyncEvent, expected: &[AssetUpdate]) {
    assert_eq!(
        event.updates.len(),
        expected.len(),
        "one update per committed asset: {:?}",
        event.updates
    );
    for update in expected {
        assert!(
            event.updates.contains(update),
            "missing {update:?} in {:?}",
            event.updates
        );
    }
}

/// The malformed body every failing-asset test serves.
fn malformed_body() -> Vec<u8> {
    b"this,is,not,the,schema\n1,2,3,4,5\n".to_vec()
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
    let sf_la = games.iter().find(|g| g.game_id == "2025_05_SF_LA").unwrap();
    assert_eq!(sf_la.kickoff, Some(datetime!(2025-10-03 00:15 UTC)));
    let sb = games
        .iter()
        .find(|g| g.game_id == "2025_22_SEA_NE")
        .unwrap();
    assert_eq!(sb.season_week.season, Season(2025));
    assert_eq!(sb.season_week.week, Week(22));
    assert!(sb.season_week.postseason());

    // Players.
    let players = nfl.players().await.unwrap();
    assert_eq!(players.len(), 2);
    assert_eq!(players[1].full_name, "Jordan Love");

    // Weekly roster: Thielen syncs onto the team he was on that week, so the
    // week-13 MIN row and the week-14 PIT row are both retrievable.
    let min_13 = nfl
        .weekly_roster(week(2025, 13), &TeamAbbr::MIN)
        .await
        .unwrap();
    assert_eq!(min_13.len(), 1);
    assert_eq!(min_13[0].full_name, "Adam Thielen");
    let pit_14 = nfl
        .weekly_roster(week(2025, 14), &TeamAbbr::PIT)
        .await
        .unwrap();
    assert_eq!(pit_14.len(), 1);
    assert_eq!(pit_14[0].full_name, "Adam Thielen");
    assert!(
        nfl.weekly_roster(week(2025, 13), &TeamAbbr::PIT)
            .await
            .unwrap()
            .is_empty(),
        "not yet traded in week 13"
    );

    // Player week stats.
    let stats = nfl.player_week_stats(week(2025, 1)).await.unwrap();
    assert_eq!(stats.len(), 2);
    let rodgers = &stats[0];
    assert_eq!(rodgers.gsis_id, "00-0023459");
    assert_eq!(rodgers.passing_yards, 244);
    let prater = &stats[1];
    assert_eq!(prater.gsis_id, "00-0023853");
    assert_eq!(prater.team, TeamAbbr::BUF);
    assert_eq!(prater.rushing_yards, 0);

    // Team week stats are pbp-derived: the one game yields both teams' DST
    // rows (ordered by team). points_allowed is the OPPONENT's offensive points
    // computed at ingest -- ARI allowed NO's 6, NO allowed ARI's 7+3=10.
    let team = nfl.team_week_stats(week(2025, 1)).await.unwrap();
    assert_eq!(team.len(), 2);
    assert_eq!(team[0].team, TeamAbbr::ARI);
    assert_eq!(team[0].opponent, TeamAbbr::NO);
    assert_eq!(team[0].sacks, 1);
    assert_eq!(team[0].points_allowed, 6);
    assert_eq!(team[1].team, TeamAbbr::NO);
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

    let stats = nfl.player_week_stats(week(2025, 1)).await.unwrap();
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
    team.body = malformed_body();
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
    let team = nfl.team_week_stats(week(2025, 1)).await.unwrap();
    assert_eq!(team.len(), 2);
    assert_eq!(team[0].team, TeamAbbr::ARI);
    assert_eq!(team[0].points_allowed, 6);
}

#[tokio::test]
async fn one_bad_season_does_not_block_other_seasons_in_the_same_dataset() {
    // Two seasons in the window for a per-season dataset (weekly rosters):
    // 2024's asset is malformed and fails to parse, 2025's asset is valid.
    // Before the fix, the ascending-season loop tried 2024 first, hit `?`, and
    // aborted before ever attempting 2025 — so 2025's perfectly good data
    // never got stored. After the fix, both assets are attempted, so 2025's
    // weekly roster is stored even though the dataset is reported Failed
    // overall.
    let mut assets = fixtures();
    assets.push(FakeAsset {
        tag: "weekly_rosters",
        name: "roster_weekly_2024.csv",
        updated_at: "2026-06-19T00:00:00Z",
        body: malformed_body(),
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
            "weekly_rosters" => assert!(
                matches!(d.status, DatasetStatus::Failed(_)),
                "expected weekly_rosters Failed, got {:?}",
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
    let min_13 = nfl
        .weekly_roster(week(2025, 13), &TeamAbbr::MIN)
        .await
        .unwrap();
    assert_eq!(min_13.len(), 1);
    assert_eq!(min_13[0].full_name, "Adam Thielen");
}

#[tokio::test]
async fn sync_event_reports_commits_despite_failed_dataset() {
    // The 2025 stats asset is malformed and fails while the 2026 one commits:
    // the dataset is Failed, yet the 2026 commit still reaches the event.
    let mut assets = fixtures();
    assets.push(FakeAsset {
        tag: "stats_player",
        name: "stats_player_week_2026.csv",
        updated_at: "2026-01-20T00:00:00Z",
        body: include_bytes!("fixtures/stats_player_week_2026.csv").to_vec(),
    });
    let stats_2025 = assets
        .iter_mut()
        .find(|a| a.name == "stats_player_week_2025.csv")
        .unwrap();
    stats_2025.body = malformed_body();
    let server = mock_nflverse(&assets).await;
    let dir = tempfile::tempdir().unwrap();
    let (nfl, mut events) = connect_with_events(config(&server, &dir)).await;

    let report = nfl.sync().await.unwrap();
    let event = events.try_recv().expect("one completion event per sync");

    assert!(!event.all_ok);
    assert!(
        event.updates.contains(&AssetUpdate {
            dataset: Dataset::PlayerWeekStats,
            season: Some(Season(2026)),
        }),
        "the committing 2026 asset must be reported despite the failed dataset: {:?}",
        event.updates
    );
    assert!(
        !event.updates.contains(&AssetUpdate {
            dataset: Dataset::PlayerWeekStats,
            season: Some(Season(2025)),
        }),
        "the failed 2025 asset must not be reported: {:?}",
        event.updates
    );
    // The failed dataset did not erase its siblings' updates.
    assert_updates(
        &event,
        &[
            AssetUpdate {
                dataset: Dataset::Schedules,
                season: None,
            },
            AssetUpdate {
                dataset: Dataset::Players,
                season: None,
            },
            AssetUpdate {
                dataset: Dataset::WeeklyRosters,
                season: Some(Season(2025)),
            },
            AssetUpdate {
                dataset: Dataset::Pbp,
                season: Some(Season(2025)),
            },
            AssetUpdate {
                dataset: Dataset::PlayerWeekStats,
                season: Some(Season(2026)),
            },
        ],
    );

    let stats_report = dataset_report(&report, Dataset::PlayerWeekStats);
    assert!(
        matches!(stats_report.status, DatasetStatus::Failed(_)),
        "expected player_week_stats failed, got {:?}",
        stats_report.status
    );

    // The committed 2026 rows are stored.
    let stored_2026_stats = nfl.player_week_stats(week(2026, 1)).await.unwrap();
    assert!(!stored_2026_stats.is_empty());
    assert!(events.try_recv().is_err(), "exactly one event per sync");
}

#[tokio::test]
async fn sync_event_reports_unchanged_completion() {
    // A 2024 roster asset joins the 2025 fixtures: each historical import
    // reports its own year, shared datasets report none.
    let mut assets = fixtures();
    assets.push(FakeAsset {
        tag: "weekly_rosters",
        name: "roster_weekly_2024.csv",
        updated_at: "2026-06-19T00:00:00Z",
        body: include_bytes!("fixtures/roster_weekly_2024.csv").to_vec(),
    });
    let server = mock_nflverse(&assets).await;
    let dir = tempfile::tempdir().unwrap();
    let (nfl, mut events) = connect_with_events(NflverseDataConfig {
        earliest_season: 2024,
        ..config(&server, &dir)
    })
    .await;

    let report = nfl.sync().await.unwrap();
    assert_all_updated(&report);
    let event = events.try_recv().expect("one completion event per sync");
    assert!(event.all_ok);
    assert_updates(
        &event,
        &[
            AssetUpdate {
                dataset: Dataset::Schedules,
                season: None,
            },
            AssetUpdate {
                dataset: Dataset::Players,
                season: None,
            },
            AssetUpdate {
                dataset: Dataset::WeeklyRosters,
                season: Some(Season(2024)),
            },
            AssetUpdate {
                dataset: Dataset::WeeklyRosters,
                season: Some(Season(2025)),
            },
            AssetUpdate {
                dataset: Dataset::PlayerWeekStats,
                season: Some(Season(2025)),
            },
            AssetUpdate {
                dataset: Dataset::Pbp,
                season: Some(Season(2025)),
            },
        ],
    );

    // The second sync changes nothing, yet still completes with an empty
    // event.
    let report = nfl.sync().await.unwrap();
    for d in &report.datasets {
        assert!(
            matches!(d.status, DatasetStatus::Unchanged),
            "expected {} unchanged, got {:?}",
            d.dataset.name(),
            d.status
        );
    }
    let unchanged_event = events.try_recv().expect("one completion event per sync");
    assert!(unchanged_event.all_ok);
    assert!(unchanged_event.updates.is_empty());
    assert!(events.try_recv().is_err(), "exactly one event per sync");
}

#[tokio::test]
async fn sync_event_excludes_failed_assets() {
    // Every asset is malformed: all datasets fail, and the completion event
    // still fires with no updates.
    let assets: Vec<FakeAsset> = fixtures()
        .into_iter()
        .map(|mut asset| {
            asset.body = malformed_body();
            asset
        })
        .collect();
    let server = mock_nflverse(&assets).await;
    let dir = tempfile::tempdir().unwrap();
    let (nfl, mut events) = connect_with_events(config(&server, &dir)).await;

    let report = nfl.sync().await.unwrap();
    assert!(!report.all_ok());
    for d in &report.datasets {
        assert!(
            matches!(d.status, DatasetStatus::Failed(_)),
            "expected {} failed, got {:?}",
            d.dataset.name(),
            d.status
        );
    }
    let event = events
        .try_recv()
        .expect("fully failed syncs still complete with an event");
    assert!(!event.all_ok);
    assert!(
        event.updates.is_empty(),
        "no commits, so no updates: {:?}",
        event.updates
    );
    assert!(events.try_recv().is_err(), "exactly one event per sync");
}

#[tokio::test]
async fn sync_event_reports_zero_row_replacements() {
    let server = mock_nflverse(&fixtures()).await;
    let dir = tempfile::tempdir().unwrap();
    let nfl = NflverseData::connect(config(&server, &dir)).await.unwrap();
    nfl.sync().await.unwrap();
    drop(nfl);
    drop(server);

    // Bump the stats asset but serve only its header: the replacement commits
    // zero rows, and is still a commit.
    let mut assets = fixtures();
    let stats = assets.iter_mut().find(|a| a.tag == "stats_player").unwrap();
    stats.updated_at = "2026-01-16T00:00:00Z";
    let header = String::from_utf8(stats.body.clone())
        .unwrap()
        .lines()
        .next()
        .unwrap()
        .to_string();
    stats.body = header.into_bytes();
    let server = mock_nflverse(&assets).await;
    let (nfl, mut events) = connect_with_events(NflverseDataConfig {
        github_api_base: server.uri(),
        ..config(&server, &dir)
    })
    .await;

    let report = nfl.sync().await.unwrap();
    let stats_report = dataset_report(&report, Dataset::PlayerWeekStats);
    assert!(
        matches!(
            stats_report.status,
            DatasetStatus::Updated { assets: 1, rows: 0 }
        ),
        "expected a zero-row update, got {:?}",
        stats_report.status
    );
    let event = events.try_recv().expect("one completion event per sync");
    assert!(event.all_ok);
    assert!(
        event.updates.contains(&AssetUpdate {
            dataset: Dataset::PlayerWeekStats,
            season: Some(Season(2025)),
        }),
        "a zero-row replacement is still a committed asset: {:?}",
        event.updates
    );
    assert_eq!(event.updates.len(), 1);
    assert!(
        nfl.player_week_stats(week(2025, 1))
            .await
            .unwrap()
            .is_empty(),
        "the zero-row replacement wiped the season"
    );
    assert!(events.try_recv().is_err(), "exactly one event per sync");
}

#[tokio::test]
async fn sync_event_closed_receiver_is_ignored() {
    let server = mock_nflverse(&fixtures()).await;
    let dir = tempfile::tempdir().unwrap();
    let (tx, rx) = mpsc::unbounded_channel();
    drop(rx);
    let nfl = NflverseData::connect_with_sync_events(config(&server, &dir), Some(tx))
        .await
        .unwrap();

    // A closed receiver is not an error.
    let report = nfl.sync().await.unwrap();
    assert_all_updated(&report);
}

#[tokio::test]
async fn sync_notify_channel_keeps_success_only_semantics() {
    let server = mock_nflverse(&fixtures()).await;
    let dir = tempfile::tempdir().unwrap();
    let (tx, mut notify) = mpsc::unbounded_channel::<()>();
    let nfl = NflverseData::connect_with_sync_notify(config(&server, &dir), Some(tx.clone()))
        .await
        .unwrap();

    nfl.sync().await.unwrap();
    notify
        .try_recv()
        .expect("notified after the fresh successful sync");
    nfl.sync().await.unwrap(); // unchanged
    notify
        .try_recv()
        .expect("unchanged syncs still notify the legacy channel");
    assert!(notify.try_recv().is_err(), "one notification per sync");
    drop(nfl);
    drop(server);

    // A partially failed sync completes without notifying the legacy channel.
    let mut assets = fixtures();
    let pbp = assets.iter_mut().find(|a| a.tag == "pbp").unwrap();
    pbp.updated_at = "2026-01-16T00:00:00Z";
    pbp.body = malformed_body();
    let server = mock_nflverse(&assets).await;
    let nfl = NflverseData::connect_with_sync_notify(
        NflverseDataConfig {
            github_api_base: server.uri(),
            ..config(&server, &dir)
        },
        Some(tx),
    )
    .await
    .unwrap();

    let report = nfl.sync().await.unwrap();
    assert!(!report.all_ok());
    assert!(
        notify.try_recv().is_err(),
        "partial failures must not notify the legacy channel"
    );
}
