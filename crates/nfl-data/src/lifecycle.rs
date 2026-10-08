#[cfg(test)]
mod tests {
    use std::time::Duration;

    use crate::{
        NflData,
        test_support::{mock_nfl, mock_nfl_delayed},
    };
    use utils::background::RunnerState;

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
            let (nfl, server, _dir) = mock_nfl(interval).await;
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
        let (nfl, _server, _dir) = mock_nfl_delayed(Duration::from_millis(200)).await;
        assert!(nfl.request_refresh().await.unwrap());
        tokio::time::timeout(Duration::from_secs(1), async {
            while !matches!(nfl.refresh_status(), RunnerState::Running { .. }) {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        nfl.stop_background_tasks();
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(matches!(nfl.refresh_status(), RunnerState::Running { .. }));
        tokio::time::timeout(Duration::from_secs(3), async {
            while matches!(nfl.refresh_status(), RunnerState::Running { .. }) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert!(matches!(
            nfl.refresh_status(),
            RunnerState::Idle { last: Some(_) }
        ));
    }

    #[tokio::test]
    async fn lifecycle_waits_one_interval_and_does_not_duplicate_timers() {
        let interval = Duration::from_millis(150);
        let (nfl, server, _dir) = mock_nfl(Some(interval)).await;
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
        let (nfl, server, _dir) = mock_nfl(Some(interval)).await;
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
}
