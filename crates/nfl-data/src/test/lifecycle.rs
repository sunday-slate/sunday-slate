#[cfg(test)]
mod tests {
    use std::str::FromStr;
    use std::time::Duration;

    use crate::scoring::score_player;
    use crate::slates::SlateId;
    use crate::store::player_link::{self, PlayerLink};
    use crate::test::test_support::{MockAsset, NflverseMock, mock_nfl, mock_nfl_delayed};
    use crate::{
        Game, NflData, NflDataConfig, Player, PlayerWeekStats, Season, SeasonType, SeasonWeek,
        TeamAbbr, Week,
    };
    use nfl_model::NflPlayerId;
    use sqlx::Connection;
    use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode};
    use tempfile::TempDir;
    use time::macros::datetime;
    use utils::background::{LastRun, RunnerState};

    async fn wait_for_requests(server: &wiremock::MockServer, expected: usize) {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if server.received_requests().await.unwrap().len() >= expected {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("expected requests");
    }

    #[tokio::test]
    async fn construction_and_disabled_lifecycle_make_no_requests() {
        for interval in [None, Some(Duration::ZERO)] {
            let (nfl, server) = mock_nfl(interval).await;
            nfl.start_background_tasks();
            nfl.start_background_tasks();
            nfl.stop_background_tasks();
            nfl.stop_background_tasks();
            tokio::time::sleep(Duration::from_millis(50)).await;
            assert!(server.received_requests().await.unwrap().is_empty());
        }
        let _ = NflData::in_memory().await.unwrap();
    }

    #[tokio::test]
    async fn stopping_services_does_not_cancel_running_refresh() {
        let (nfl, _server) = mock_nfl_delayed(Duration::from_millis(200)).await;
        assert!(nfl.provider.request_refresh().await.unwrap());
        tokio::time::timeout(Duration::from_secs(1), async {
            while !matches!(nfl.provider.refresh_status(), RunnerState::Running { .. }) {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        nfl.stop_background_tasks();
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(matches!(
            nfl.provider.refresh_status(),
            RunnerState::Running { .. }
        ));
        tokio::time::timeout(Duration::from_secs(3), async {
            while matches!(nfl.provider.refresh_status(), RunnerState::Running { .. }) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert!(matches!(
            nfl.provider.refresh_status(),
            RunnerState::Idle { last: Some(_) }
        ));
    }

    #[tokio::test]
    async fn lifecycle_waits_one_interval_and_does_not_duplicate_timers() {
        let interval = Duration::from_millis(150);
        let (nfl, server) = mock_nfl(Some(interval)).await;
        nfl.start_background_tasks();
        nfl.start_background_tasks();
        tokio::time::sleep(Duration::from_millis(60)).await;
        assert!(server.received_requests().await.unwrap().is_empty());
        wait_for_requests(&server, 5).await;
        tokio::time::sleep(Duration::from_millis(40)).await;
        assert_eq!(server.received_requests().await.unwrap().len(), 5);
        nfl.stop_background_tasks();
    }

    #[tokio::test]
    async fn lifecycle_stop_and_restart_use_the_configured_interval() {
        let interval = Duration::from_millis(150);
        let (nfl, server) = mock_nfl(Some(interval)).await;
        nfl.start_background_tasks();
        wait_for_requests(&server, 5).await;
        nfl.stop_background_tasks();
        tokio::time::sleep(interval * 2).await;
        assert_eq!(server.received_requests().await.unwrap().len(), 5);
        nfl.start_background_tasks();
        tokio::time::sleep(Duration::from_millis(60)).await;
        assert_eq!(server.received_requests().await.unwrap().len(), 5);
        wait_for_requests(&server, 10).await;
        nfl.stop_background_tasks();
    }

    // ---- the ordered update worker ----

    const WORKER_WEEK_NUM: u8 = 14;
    /// Kickoff far past any plausible test clock, so reconciliation stays
    /// tentative.
    const WORKER_KICKOFF: time::OffsetDateTime = datetime!(2030-01-06 17:00 UTC);
    /// One hour later on the same Sunday, still inside the slate window, so
    /// only the kickoff changes.
    const WORKER_KICKOFF_2: time::OffsetDateTime = datetime!(2030-01-06 18:00 UTC);
    const PREVIOUS_SEASON: Season = Season(2025);
    /// Release `updated_at` stamps the fixtures advance between syncs.
    const RELEASE_AT_1: &str = "2026-03-01T00:00:00Z";
    const RELEASE_AT_2: &str = "2026-03-02T00:00:00Z";

    const WORKER_SALARIES_CSV: &str = "\
Id,Position,First Name,Nickname,Last Name,FPPG,Played,Salary,Game,Team,Opponent,Injury Indicator,Injury Details,Tier,,,Roster Position
9001-1,RB,Mack,Mack Runner,Runner,,0,9000,KC@BUF,KC,BUF,,,,,,RB/FLEX
";

    fn worker_week() -> SeasonWeek {
        SeasonWeek {
            season: Season(2026),
            season_type: SeasonType::Reg,
            week: Week(WORKER_WEEK_NUM),
        }
    }

    fn worker_slate_id() -> SlateId {
        SlateId(i64::from(worker_week().encode()) * 10 + 1)
    }
    fn mack_id() -> NflPlayerId {
        NflPlayerId("00-MACK".into())
    }

    /// The fixture's salary export with a different salary.
    fn worker_salaries_with(mack: u32) -> String {
        WORKER_SALARIES_CSV.replace("Runner,,0,9000", &format!("Runner,,0,{mack}"))
    }

    fn worker_mack() -> Player {
        Player {
            gsis_id: "00-MACK".into(),
            espn_id: None,
            full_name: "Mack Runner".into(),
            first_name: Some("Mack".into()),
            last_name: Some("Runner".into()),
            position: Some("RB".into()),
            latest_team: Some(TeamAbbr::KC),
            headshot_url: None,
        }
    }

    fn worker_mack_game() -> Game {
        Game {
            game_id: format!("2026_{WORKER_WEEK_NUM}_BUF_KC"),
            season_week: worker_week(),
            kickoff: Some(WORKER_KICKOFF),
            home_team: TeamAbbr::BUF,
            away_team: TeamAbbr::KC,
            home_score: None,
            away_score: None,
        }
    }

    fn worker_mack_stats() -> PlayerWeekStats {
        worker_mack_stats_with(150)
    }

    fn worker_mack_stats_with(rushing_yards: i32) -> PlayerWeekStats {
        PlayerWeekStats {
            season: Season(2026),
            week: Week(WORKER_WEEK_NUM - 1),
            season_type: SeasonType::Reg,
            gsis_id: "00-MACK".into(),
            team: TeamAbbr::KC,
            opponent: Some(TeamAbbr::BUF),
            completions: 0,
            attempts: 0,
            passing_yards: 0,
            passing_tds: 0,
            passing_interceptions: 0,
            rushing_attempts: 1,
            rushing_yards,
            rushing_tds: 1,
            targets: 0,
            receptions: 0,
            receiving_yards: 0,
            receiving_tds: 0,
            fumbles_lost: 0,
            two_point_conversions: 0,
            special_teams_tds: 0,
            fumble_recovery_tds: 0,
        }
    }

    /// The comparison stat row the mock's 2025 asset and the seeding agree on.
    fn comparison_stats(rushing_yards: i32) -> PlayerWeekStats {
        PlayerWeekStats {
            season: PREVIOUS_SEASON,
            week: Week(1),
            ..worker_mack_stats_with(rushing_yards)
        }
    }

    fn previous_week() -> SeasonWeek {
        SeasonWeek {
            season: PREVIOUS_SEASON,
            season_type: SeasonType::Reg,
            week: Week(WORKER_WEEK_NUM),
        }
    }

    /// The worker fixture's seeding: one salaried player, its schedule game,
    /// the salary row, the link, and the preceding week's stats. The slate
    /// tables stay empty: the worker's first pass populates them.
    async fn seed_worker_sources(nfl: &NflData) {
        nfl.seed_for_test(&[worker_mack()], &[worker_mack_game()])
            .await
            .expect("seed provider");
        let rows = nfl
            .parse_fanduel_csv(worker_week(), WORKER_SALARIES_CSV.as_bytes())
            .expect("parse salaries");
        nfl.save_fanduel_salaries(&rows)
            .await
            .expect("save salaries");
        player_link::save_links(
            &nfl.store,
            &[PlayerLink {
                fd_player_id: 1,
                gsis_player_id: "00-MACK".into(),
            }],
        )
        .await
        .expect("save links");
        nfl.seed_week_stats_for_test(Season(2026), &[worker_mack_stats()], &[])
            .await
            .expect("seed week stats");
    }

    /// The older comparison assets the snapshot must never see: a 2025 stat
    /// row and a 2025 salary row on the same matchup. Neither save notifies.
    async fn seed_comparison_assets(nfl: &NflData) {
        nfl.seed_week_stats_for_test(Season(2025), &[comparison_stats(111)], &[])
            .await
            .expect("seed 2025 stats");
        let rows = nfl
            .parse_fanduel_csv(previous_week(), WORKER_SALARIES_CSV.as_bytes())
            .expect("parse 2025 salaries");
        nfl.save_fanduel_salaries(&rows)
            .await
            .expect("save 2025 salaries");
    }

    /// An in-memory handle in the configured season pointing at `mock`.
    async fn mock_nfl_data(mock: &NflverseMock, refresh_interval: Option<Duration>) -> NflData {
        NflData::connect(NflDataConfig {
            nflverse_database_url: "sqlite::memory:".into(),
            fanduel_database_url: "sqlite::memory:".into(),
            database_url: "sqlite::memory:".into(),
            season: Season(2026),
            earliest_season: 2020,
            github_api_base: mock.uri(),
            refresh_interval,
            ..NflDataConfig::default()
        })
        .await
        .expect("in-memory handle against the mock")
    }
    const MOCK_GAMES_HEADER: &str =
        "game_id,season,game_type,week,gameday,gametime,away_team,away_score,home_team,home_score";
    const MOCK_PLAYERS_HEADER: &str =
        "gsis_id,espn_id,display_name,first_name,last_name,position,latest_team,headshot";
    /// The nflverse stats_player_week columns the ingest maps.
    const MOCK_STATS_HEADER: &str = "player_id,season,week,season_type,team,opponent_team,completions,attempts,passing_yards,passing_tds,passing_interceptions,passing_2pt_conversions,carries,rushing_yards,rushing_tds,rushing_fumbles_lost,rushing_2pt_conversions,receptions,targets,receiving_yards,receiving_tds,receiving_fumbles_lost,receiving_2pt_conversions,sack_fumbles_lost,special_teams_tds,fumble_recovery_tds";

    /// The provider's week-14 schedule row with a fixed kickoff: `gametime` is
    /// US Eastern, so both fixture kickoffs sit in the Sunday slate window.
    fn mock_games_csv(gametime: &str) -> String {
        format!("{MOCK_GAMES_HEADER}\n2026_14_BUF_KC,2026,REG,14,2030-01-06,{gametime},KC,,BUF,\n")
    }

    fn mock_players_csv(display_name: &str) -> String {
        format!("{MOCK_PLAYERS_HEADER}\n00-MACK,,{display_name},Mack,Runner,RB,KC,\n")
    }

    /// One rushing stat row in the nflverse stats export's shape.
    fn mock_stats_csv(
        season: u16,
        week: u8,
        player_id: &str,
        team: &str,
        opponent: &str,
        rushing_yards: u32,
    ) -> String {
        format!(
            "{MOCK_STATS_HEADER}\n\
             {player_id},{season},{week},REG,{team},{opponent},\
             0,0,0,0,0,0,1,{rushing_yards},1,0,0,0,0,0,0,0,0,0,0,0\n"
        )
    }

    /// Poll until `pred` holds, panicking with `what` after five seconds.
    async fn poll_until(what: &'static str, mut pred: impl AsyncFnMut() -> bool) {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if pred().await {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect(what);
    }

    /// Wait until the provider's refresh runner is idle with a fully
    /// successful last run. A partially failed sync also parks the runner in
    /// `Idle`, so this distinguishes committed syncs from failed ones.
    async fn wait_for_successful_sync(nfl: &NflData, what: &'static str) {
        poll_until(what, || async {
            matches!(
                nfl.provider.refresh_status(),
                RunnerState::Idle {
                    last: Some(LastRun { outcome: Ok(_), .. }),
                }
            )
        })
        .await;
    }

    fn worker_abort_handle(nfl: &NflData) -> tokio::task::AbortHandle {
        nfl.update_worker
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
            .expect("the worker is spawned")
            .abort_handle()
    }

    /// A raw connection to a database file the handle's pools do not own.
    async fn raw_pool(url: &str) -> sqlx::SqlitePool {
        sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::from_str(url)
                    .expect("valid url")
                    .journal_mode(SqliteJournalMode::Wal),
            )
            .await
            .expect("raw connection")
    }

    /// Hold the store's WAL writer lock from an outside connection, so the
    /// worker's next reconcile transaction pauses at `BEGIN IMMEDIATE`.
    /// Dropping the guard closes the connection and releases the lock.
    async fn hold_writer_lock(url: &str) -> sqlx::SqliteConnection {
        let mut conn = sqlx::SqliteConnection::connect_with(
            &SqliteConnectOptions::from_str(url)
                .expect("store url")
                .journal_mode(SqliteJournalMode::Wal),
        )
        .await
        .expect("raw connection");
        sqlx::query("BEGIN IMMEDIATE")
            .execute(&mut conn)
            .await
            .expect("lock the store writer");
        conn
    }
    #[tokio::test]
    async fn start_is_idempotent_and_the_worker_follows_invalidations() {
        let nfl = NflData::in_memory().await.expect("in-memory handle");
        seed_worker_sources(&nfl).await;

        // The worker's startup pass populates the slates and builds the
        // snapshot for in-memory handles, whose construction built nothing.
        nfl.start_background_tasks();
        let abort = worker_abort_handle(&nfl);
        poll_until("the startup pass built the snapshot", || async {
            nfl.snapshot()
                .player(&worker_slate_id(), &mack_id())
                .is_some_and(|mack| mack.salary == 9000)
        })
        .await;

        // A repeated start is a no-op: the same worker keeps running.
        nfl.start_background_tasks();
        assert!(
            !abort.is_finished(),
            "the first start's worker is still running"
        );

        // A local invalidation rebuilds without any sync.
        let rows = nfl
            .parse_fanduel_csv(worker_week(), worker_salaries_with(9500).as_bytes())
            .expect("parse salaries");
        nfl.save_fanduel_salaries(&rows)
            .await
            .expect("save salaries");
        poll_until("the invalidation rebuilt the snapshot", || async {
            nfl.snapshot()
                .player(&worker_slate_id(), &mack_id())
                .is_some_and(|mack| mack.salary == 9500)
        })
        .await;

        // The scheduler stop keeps the worker: it is not cancelled.
        nfl.stop_background_tasks();
        assert!(!abort.is_finished(), "the scheduler stop keeps the worker");
    }

    #[tokio::test]
    async fn queued_before_start_is_processed() {
        let mock = NflverseMock::start().await;
        mock.set_release(
            "schedules",
            vec![MockAsset::csv(
                "games.csv",
                RELEASE_AT_1,
                mock_games_csv("13:00"),
            )],
        );
        let nfl = mock_nfl_data(&mock, None).await;
        seed_worker_sources(&nfl).await;

        // A salary save commits and queues its invalidation before the worker
        // runs. The sync commits only its shared schedules asset, so its event
        // reports a failed sibling — and still queues.
        let rows = nfl
            .parse_fanduel_csv(worker_week(), worker_salaries_with(9500).as_bytes())
            .expect("parse salaries");
        nfl.save_fanduel_salaries(&rows)
            .await
            .expect("save salaries");
        assert!(
            nfl.provider
                .request_refresh()
                .await
                .expect("request refresh"),
            "the runner was idle"
        );
        poll_until("the pre-start sync settled", || async {
            matches!(
                nfl.provider.refresh_status(),
                RunnerState::Idle { last: Some(_) }
            )
        })
        .await;

        nfl.start_background_tasks();
        poll_until("the pre-start work published", || async {
            let snapshot = nfl.snapshot();
            snapshot
                .slate(&worker_slate_id())
                .is_some_and(|slate| slate.kickoff == Some(WORKER_KICKOFF_2))
                && snapshot
                    .player(&worker_slate_id(), &mack_id())
                    .is_some_and(|mack| mack.salary == 9500)
        })
        .await;
        nfl.stop_background_tasks();
    }

    #[tokio::test]
    async fn configured_season_assets_publish_but_historical_only_ones_do_not() {
        let mock = NflverseMock::start().await;
        // The 2025 stats sibling fails: the dataset reports Failed despite its
        // committed 2026 rows.
        mock.set_release(
            "schedules",
            vec![MockAsset::csv(
                "games.csv",
                RELEASE_AT_1,
                mock_games_csv("13:00"),
            )],
        );
        mock.set_release(
            "players",
            vec![MockAsset::csv(
                "players.csv",
                RELEASE_AT_1,
                mock_players_csv("Mack R. Runner"),
            )],
        );
        mock.set_release(
            "stats_player",
            vec![
                MockAsset::failing("stats_player_week_2025.csv", RELEASE_AT_1, 500),
                MockAsset::csv(
                    "stats_player_week_2026.csv",
                    RELEASE_AT_1,
                    mock_stats_csv(2026, WORKER_WEEK_NUM - 1, "00-MACK", "KC", "BUF", 300),
                ),
            ],
        );

        let nfl = mock_nfl_data(&mock, None).await;
        seed_worker_sources(&nfl).await;
        seed_comparison_assets(&nfl).await;
        nfl.start_background_tasks();

        // The baseline publishes the seeded sources: the 2025 comparison rows
        // never reach it.
        poll_until("the startup pass published", || async {
            nfl.snapshot()
                .slate(&worker_slate_id())
                .is_some_and(|slate| slate.kickoff == Some(WORKER_KICKOFF))
        })
        .await;
        let before_fppg = score_player(&worker_mack_stats()).total;
        let baseline = nfl.snapshot();
        let mack = baseline
            .player(&worker_slate_id(), &mack_id())
            .expect("the baseline mack");
        assert_eq!(
            (mack.salary, mack.name.as_str(), mack.fppg),
            (9000, "Mack Runner", before_fppg)
        );
        assert_eq!(baseline.slates().count(), 1, "only the configured season");

        // A partial refresh publishes the committed 2026 stats, schedules, and
        // players assets despite the failing 2025 sibling.
        assert!(
            nfl.provider
                .request_refresh()
                .await
                .expect("request refresh"),
            "the runner was idle"
        );
        let after_fppg = score_player(&worker_mack_stats_with(300)).total;
        assert_ne!(before_fppg, after_fppg, "the fixture changes the stats");
        poll_until("the committed 2026 assets published", || async {
            let snapshot = nfl.snapshot();
            snapshot
                .player(&worker_slate_id(), &mack_id())
                .is_some_and(|mack| mack.fppg == after_fppg && mack.name == "Mack R. Runner")
                && snapshot
                    .slate(&worker_slate_id())
                    .is_some_and(|slate| slate.kickoff == Some(WORKER_KICKOFF_2))
                && snapshot.slates().count() == 1
        })
        .await;

        // Now only the 2025 stats asset advances. The historical import updates
        // the historical rows...
        mock.set_release(
            "stats_player",
            vec![
                MockAsset::csv(
                    "stats_player_week_2025.csv",
                    RELEASE_AT_2,
                    mock_stats_csv(2025, 1, "00-2025RB", "KC", "BUF", 20),
                ),
                MockAsset::csv(
                    "stats_player_week_2026.csv",
                    RELEASE_AT_1,
                    mock_stats_csv(2026, WORKER_WEEK_NUM - 1, "00-MACK", "KC", "BUF", 300),
                ),
            ],
        );
        assert!(
            nfl.provider
                .request_refresh()
                .await
                .expect("request refresh"),
            "the runner was idle"
        );
        wait_for_successful_sync(&nfl, "the historical-only sync succeeded").await;
        let stats_2025 = nfl
            .player_week_stats(PREVIOUS_SEASON, Week(1))
            .await
            .expect("read 2025 stats");
        assert_eq!(
            (stats_2025.len(), stats_2025[0].rushing_yards),
            (1, 20),
            "the historical asset committed"
        );

        // ...while the published contents stay untouched: the historical-only
        // event cannot rebuild a snapshot the configured season did not change.
        tokio::time::sleep(Duration::from_millis(300)).await;
        let published = nfl.snapshot();
        let mack = published
            .player(&worker_slate_id(), &mack_id())
            .expect("the published mack");
        assert_eq!((mack.salary, mack.fppg), (9000, after_fppg));
        assert_eq!(
            published
                .slate(&worker_slate_id())
                .and_then(|slate| slate.kickoff),
            Some(WORKER_KICKOFF_2)
        );

        // A later local invalidation still publishes: the worker is alive.
        let rows = nfl
            .parse_fanduel_csv(worker_week(), worker_salaries_with(9500).as_bytes())
            .expect("parse salaries");
        nfl.save_fanduel_salaries(&rows)
            .await
            .expect("save salaries");
        poll_until("the post-refresh invalidation published", || async {
            nfl.snapshot()
                .player(&worker_slate_id(), &mack_id())
                .is_some_and(|mack| mack.salary == 9500)
        })
        .await;
        nfl.stop_background_tasks();
    }

    #[tokio::test]
    async fn stopping_scheduler_keeps_downstream_updates() {
        let mock = NflverseMock::start().await;
        mock.set_release(
            "schedules",
            vec![MockAsset::csv(
                "games.csv",
                RELEASE_AT_1,
                mock_games_csv("12:00"),
            )],
        );
        mock.set_release(
            "players",
            vec![MockAsset::csv(
                "players.csv",
                RELEASE_AT_1,
                mock_players_csv("Mack Runner"),
            )],
        );
        mock.set_release(
            "stats_player",
            vec![
                MockAsset::csv(
                    "stats_player_week_2026.csv",
                    RELEASE_AT_1,
                    mock_stats_csv(2026, WORKER_WEEK_NUM - 1, "00-MACK", "KC", "BUF", 150),
                ),
                MockAsset::csv(
                    "stats_player_week_2025.csv",
                    RELEASE_AT_1,
                    mock_stats_csv(2025, 1, "00-2025RB", "KC", "BUF", 10),
                ),
            ],
        );

        let nfl = mock_nfl_data(&mock, Some(Duration::from_millis(150))).await;
        seed_worker_sources(&nfl).await;
        nfl.start_background_tasks();
        poll_until("the startup pass published", || async {
            nfl.snapshot()
                .slate(&worker_slate_id())
                .is_some_and(|slate| slate.kickoff == Some(WORKER_KICKOFF))
        })
        .await;

        // Slow the mock, then advance only the schedules asset: whichever
        // refresh runs next carries the new kickoff while it is running.
        mock.set_delay(Duration::from_millis(100));
        mock.set_release(
            "schedules",
            vec![MockAsset::csv(
                "games.csv",
                RELEASE_AT_2,
                mock_games_csv("13:00"),
            )],
        );
        let _ = nfl
            .provider
            .request_refresh()
            .await
            .expect("request refresh");
        poll_until("the refresh is running", || async {
            matches!(nfl.provider.refresh_status(), RunnerState::Running { .. })
        })
        .await;

        // Stopping the scheduler does not cancel the running refresh: its sync
        // completes and its event still flows.
        nfl.stop_background_tasks();
        poll_until("the stopped scheduler's refresh published", || async {
            nfl.snapshot()
                .slate(&worker_slate_id())
                .is_some_and(|slate| slate.kickoff == Some(WORKER_KICKOFF_2))
        })
        .await;

        // Salary changes still publish after the scheduler stopped.
        let rows = nfl
            .parse_fanduel_csv(worker_week(), worker_salaries_with(9500).as_bytes())
            .expect("parse salaries");
        nfl.save_fanduel_salaries(&rows)
            .await
            .expect("save salaries");
        poll_until("the post-stop salary change published", || async {
            nfl.snapshot()
                .player(&worker_slate_id(), &mack_id())
                .is_some_and(|mack| mack.salary == 9500)
        })
        .await;
    }

    #[tokio::test]
    async fn dropping_owner_stops_update_worker() {
        let dir = TempDir::new().expect("temp dir");
        let url = |name: &str| format!("sqlite:{}", dir.path().join(name).display());
        let nfl = NflData::connect(NflDataConfig {
            nflverse_database_url: url("provider.db"),
            fanduel_database_url: url("fanduel.db"),
            database_url: url("store.db"),
            season: Season(2026),
            ..Default::default()
        })
        .await
        .expect("connect");

        // A seeded game gives the first pass something to write; the lock
        // makes that write wait, so the worker is mid-pass at the drop.
        nfl.seed_for_test(&[], &[worker_mack_game()])
            .await
            .expect("seed provider");
        let lock = hold_writer_lock(&url("store.db")).await;
        nfl.start_background_tasks();
        let abort = worker_abort_handle(&nfl);
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!abort.is_finished(), "the worker is alive while paused");

        drop(nfl);
        drop(lock);
        tokio::time::timeout(Duration::from_secs(3), async {
            while !abort.is_finished() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the update worker cancelled");

        // Releasing the lock did not resume the pass: the task is gone.
        tokio::time::sleep(Duration::from_millis(200)).await;
        let pool = raw_pool(&url("store.db")).await;
        let memberships: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM slate_games")
            .fetch_one(&pool)
            .await
            .expect("count");
        pool.close().await;
        assert_eq!(memberships, 0, "the cancelled worker wrote nothing");
    }
}
