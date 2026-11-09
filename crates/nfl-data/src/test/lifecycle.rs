#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use crate::scoring::score_player;
    use crate::slates::SlateId;
    use crate::store::player_link::{self, PlayerLink};
    use crate::test::test_support::{MockAsset, NflverseMock, mock_nfl, mock_nfl_delayed};
    use crate::{
        NflData, NflDataConfig, Player, PlayerWeekStats, Season, SeasonType, SeasonWeek, TeamAbbr,
        Week,
    };
    use nfl_model::NflPlayerId;
    use time::OffsetDateTime;
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

    fn worker_abort_handle(nfl: &NflData) -> tokio::task::AbortHandle {
        nfl.update_worker
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
            .expect("the worker is spawned")
            .abort_handle()
    }

    #[tokio::test]
    async fn start_is_idempotent_and_stop_keeps_the_worker() {
        let (nfl, _server) = mock_nfl(None).await;
        nfl.start_background_tasks();
        let abort = worker_abort_handle(&nfl);

        // A repeated start is a no-op: the same worker keeps running.
        nfl.start_background_tasks();
        assert!(
            !abort.is_finished(),
            "the first start's worker is still running"
        );

        // Stopping the scheduler leaves the notification task alone.
        nfl.stop_background_tasks();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!abort.is_finished(), "the scheduler stop keeps the worker");
    }

    #[tokio::test]
    async fn dropping_the_handle_aborts_the_worker() {
        let (nfl, _server) = mock_nfl(None).await;
        nfl.start_background_tasks();
        let abort = worker_abort_handle(&nfl);

        // The task holds no strong handle, so the last Arc owner can go.
        drop(nfl);
        tokio::time::timeout(Duration::from_secs(3), async {
            while !abort.is_finished() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the notification task is aborted with its handle");
    }

    // ---- the sync notification ----

    const WEEK_NUM: u8 = 14;
    /// 13:00 Eastern on the fixture's Sunday: inside the main slate window.
    const KICKOFF: OffsetDateTime = datetime!(2030-01-06 18:00 UTC);
    const PREVIOUS_SEASON: Season = Season(2025);
    /// Release `updated_at` stamps the fixtures advance between syncs.
    const RELEASE_AT_1: &str = "2026-03-01T00:00:00Z";
    const RELEASE_AT_2: &str = "2026-03-02T00:00:00Z";

    const SALARIES_CSV: &str = "\
Id,Position,First Name,Nickname,Last Name,FPPG,Played,Salary,Game,Team,Opponent,Injury Indicator,Injury Details,Tier,,,Roster Position
9001-1,RB,Mack,Mack Runner,Runner,,0,9000,KC@BUF,KC,BUF,,,,,,RB/FLEX
";

    fn salaried_week() -> SeasonWeek {
        SeasonWeek {
            season: Season(2026),
            season_type: SeasonType::Reg,
            week: Week(WEEK_NUM),
        }
    }

    fn salaried_slate_id() -> SlateId {
        SlateId(i64::from(salaried_week().encode()) * 10 + 1)
    }

    fn mack_id() -> NflPlayerId {
        NflPlayerId("00-MACK".into())
    }

    fn mack() -> Player {
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

    fn mack_stats(rushing_yards: i32) -> PlayerWeekStats {
        PlayerWeekStats {
            season: Season(2026),
            week: Week(WEEK_NUM - 1),
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

    /// Seed what the schedule's first sync cannot bring: the player row, the
    /// salaried week's source rows, and their link.
    async fn seed_sources(nfl: &NflData) {
        nfl.seed_for_test(&[mack()], &[])
            .await
            .expect("seed provider");
        let rows = crate::salaries::parse_fanduel_csv(
            &nfl.fanduel,
            salaried_week(),
            SALARIES_CSV.as_bytes(),
        )
        .expect("parse salaries");
        crate::salaries::save_fanduel_salaries(&nfl.fanduel, &rows)
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
    }

    /// An in-memory handle in the configured season pointing at `mock`.
    async fn mock_nfl_data(
        mock: &NflverseMock,
        refresh_interval: Option<Duration>,
    ) -> Arc<NflData> {
        Arc::new(
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
            .expect("in-memory handle against the mock"),
        )
    }

    const MOCK_GAMES_HEADER: &str =
        "game_id,season,game_type,week,gameday,gametime,away_team,away_score,home_team,home_score";
    const MOCK_PLAYERS_HEADER: &str =
        "gsis_id,espn_id,display_name,first_name,last_name,position,latest_team,headshot";
    /// The nflverse stats_player_week columns the ingest maps.
    const MOCK_STATS_HEADER: &str = "player_id,season,week,season_type,team,opponent_team,completions,attempts,passing_yards,passing_tds,passing_interceptions,passing_2pt_conversions,carries,rushing_yards,rushing_tds,rushing_fumbles_lost,rushing_2pt_conversions,receptions,targets,receiving_yards,receiving_tds,receiving_fumbles_lost,receiving_2pt_conversions,sack_fumbles_lost,special_teams_tds,fumble_recovery_tds";

    /// The provider's week-14 schedule row with a fixed kickoff. `gametime` is
    /// US Eastern, so the fixture kickoff sits in the Sunday slate window.
    fn mock_games_csv(gametime: &str) -> String {
        format!(
            "{MOCK_GAMES_HEADER}\n2026_{WEEK_NUM}_BUF_KC,2026,REG,{WEEK_NUM},2030-01-06,{gametime},KC,,BUF,\n"
        )
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

    #[tokio::test]
    async fn a_relevant_sync_publishes_committed_assets_and_historical_ones_do_not() {
        let mock = NflverseMock::start().await;
        // The 2025 stats sibling fails: the event reports a failed dataset
        // despite its committed 2026 rows.
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
                    mock_stats_csv(2026, WEEK_NUM - 1, "00-MACK", "KC", "BUF", 300),
                ),
            ],
        );

        let nfl = mock_nfl_data(&mock, None).await;
        seed_sources(&nfl).await;
        nfl.start_background_tasks();
        assert!(
            nfl.snapshot().slates().next().is_none(),
            "nothing is synced yet"
        );

        // A partial refresh still publishes the committed 2026 schedules,
        // players, and stats assets.
        assert!(
            nfl.provider
                .request_refresh()
                .await
                .expect("request refresh"),
            "the runner was idle"
        );
        let fppg = score_player(&mack_stats(300)).total;
        poll_until("the committed 2026 assets published", || async {
            let snapshot = nfl.snapshot();
            snapshot
                .slate(&salaried_slate_id())
                .is_some_and(|slate| slate.kickoff == Some(KICKOFF))
                && snapshot
                    .player(&salaried_slate_id(), &mack_id())
                    .is_some_and(|mack| mack.fppg == fppg && mack.name == "Mack R. Runner")
        })
        .await;
        let published = nfl.snapshot();
        assert_eq!(published.slates().count(), 1, "only the configured season");

        // Now only the 2025 stats asset advances: the historical rows commit
        // and the slates reconcile without a rebuild.
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
                    mock_stats_csv(2026, WEEK_NUM - 1, "00-MACK", "KC", "BUF", 300),
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

        // Give the notification time to settle: it reconciles, finds no
        // visible slate change, and leaves the publication alone.
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(
            Arc::ptr_eq(&published, &nfl.snapshot()),
            "a historical-only sync republished nothing"
        );
        nfl.stop_background_tasks();
    }

    #[tokio::test]
    async fn failed_irrelevant_sync_keeps_the_published_snapshot() {
        let mock = NflverseMock::start().await;
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

        let nfl = mock_nfl_data(&mock, None).await;
        seed_sources(&nfl).await;
        nfl.start_background_tasks();
        assert!(
            nfl.provider
                .request_refresh()
                .await
                .expect("request refresh"),
            "the runner was idle"
        );
        wait_for_successful_sync(&nfl, "the initial sync succeeded").await;
        poll_until("the schedule published", || async {
            nfl.snapshot().slate(&salaried_slate_id()).is_some()
        })
        .await;
        let published = nfl.snapshot();

        // Every listed asset now fails: nothing this season can see commits.
        mock.set_release(
            "schedules",
            vec![MockAsset::failing("games.csv", RELEASE_AT_2, 500)],
        );
        mock.set_release(
            "players",
            vec![MockAsset::failing("players.csv", RELEASE_AT_2, 500)],
        );
        assert!(
            nfl.provider
                .request_refresh()
                .await
                .expect("request refresh"),
            "the runner was idle"
        );
        poll_until("the failed sync settled", || async {
            matches!(
                nfl.provider.refresh_status(),
                RunnerState::Idle {
                    last: Some(LastRun {
                        outcome: Err(_),
                        ..
                    }),
                }
            )
        })
        .await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(
            Arc::ptr_eq(&published, &nfl.snapshot()),
            "a failed irrelevant sync republished nothing"
        );
        nfl.stop_background_tasks();
    }
}
