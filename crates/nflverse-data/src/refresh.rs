use std::{
    future::Future,
    sync::{Arc, Mutex},
    time::Duration,
};

use tokio::{task::JoinHandle, time::MissedTickBehavior};
use utils::background::{Runner, RunnerState};

use crate::{DatasetFreshness, NflverseData, NflverseDataError};

/// Start-time dataset freshness while running, or the last report text when idle.
pub type RefreshStatus = RunnerState<Vec<DatasetFreshness>, String>;
type RefreshRunner = Runner<Vec<DatasetFreshness>, String>;

#[derive(Default)]
pub(crate) struct Refresh {
    runner: Arc<RefreshRunner>,
    scheduler: Mutex<Option<JoinHandle<()>>>,
}

impl Drop for Refresh {
    fn drop(&mut self) {
        if let Some(task) = self
            .scheduler
            .get_mut()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            task.abort();
        }
    }
}

impl NflverseData {
    /// Request a detached refresh. Returns false if a background refresh is running.
    /// Manual requests and scheduled requests share the same runner.
    pub async fn request_refresh(&self) -> Result<bool, NflverseDataError> {
        let runner = &self.inner.refresh.runner;
        if runner.is_running() {
            return Ok(false);
        }
        let freshness = self.freshness().await?;
        let provider = self.clone();
        Ok(runner.try_start(freshness, move |_| async move {
            match provider.sync().await {
                Ok(report) if report.all_ok() => Ok(report.to_string()),
                Ok(report) => Err(report.to_string()),
                Err(error) => Err(error.to_string()),
            }
        }))
    }

    pub fn refresh_status(&self) -> RefreshStatus {
        self.inner.refresh.runner.snapshot()
    }

    /// Start one timer per instance; the first refresh is after a full interval.
    /// Returns false if already started. Zero disables (and stops) the timer.
    /// Stop before starting again to change the interval. Construction never starts it.
    pub fn start_scheduler(&self, interval: Duration) -> bool {
        let mut scheduler = self
            .inner
            .refresh
            .scheduler
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if interval.is_zero() {
            if let Some(task) = scheduler.take() {
                task.abort();
            }
            return false;
        }
        if scheduler.as_ref().is_some_and(|task| !task.is_finished()) {
            return false;
        }
        // A sleeping timer must not keep the provider alive or form an ownership cycle.
        let inner = Arc::downgrade(&self.inner);
        *scheduler = Some(tokio::spawn(run_scheduler_with(interval, move || {
            let inner = inner.clone();
            async move {
                match inner.upgrade() {
                    Some(inner) => NflverseData { inner }.request_refresh().await,
                    None => Ok(false),
                }
            }
        })));
        true
    }

    /// Stop future timer triggers without cancelling any detached refresh in progress.
    pub fn stop_scheduler(&self) -> bool {
        let mut scheduler = self
            .inner
            .refresh
            .scheduler
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(task) = scheduler.take() {
            task.abort();
            true
        } else {
            false
        }
    }
}

async fn scheduler_tick(attempt: impl Future<Output = Result<bool, NflverseDataError>>) -> bool {
    match attempt.await {
        Ok(true) => true,
        Ok(false) => {
            tracing::info!("scheduled nfl sync skipped: a sync is already running");
            false
        }
        Err(error) => {
            tracing::warn!(error = ?error, "scheduled nfl sync could not start");
            false
        }
    }
}

async fn run_scheduler_with<F, Fut>(interval: Duration, mut attempt: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<bool, NflverseDataError>> + Send,
{
    let mut ticker = tokio::time::interval_at(tokio::time::Instant::now() + interval, interval);
    // Late polls align to the next deadline rather than replaying missed refreshes.
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        ticker.tick().await;
        scheduler_tick(attempt()).await;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::NflverseDataConfig;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };

    async fn mock_provider(fail: bool, delay: Duration) -> (NflverseData, MockServer) {
        let server = MockServer::start().await;
        for tag in [
            "schedules",
            "players",
            "rosters",
            "weekly_rosters",
            "stats_player",
            "pbp",
        ] {
            let response = if fail {
                ResponseTemplate::new(503)
            } else {
                let asset = match tag {
                    "schedules" => Some((
                        "games.csv",
                        include_bytes!("../tests/fixtures/games.csv").as_slice(),
                    )),
                    "players" => Some((
                        "players.csv",
                        include_bytes!("../tests/fixtures/players.csv").as_slice(),
                    )),
                    _ => None,
                };
                let mut assets = Vec::new();
                if let Some((name, body)) = asset {
                    let url = format!("/{name}");
                    Mock::given(method("GET"))
                        .and(path(&url))
                        .respond_with(ResponseTemplate::new(200).set_body_bytes(body))
                        .expect(1)
                        .mount(&server)
                        .await;
                    assets.push(serde_json::json!({
                        "name": name,
                        "updated_at": "2026-01-01T00:00:00Z",
                        "browser_download_url": format!("{}{url}", server.uri()),
                    }));
                }
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "assets": assets }))
            }
            .set_delay(delay);
            Mock::given(method("GET"))
                .and(path(format!(
                    "/repos/nflverse/nflverse-data/releases/tags/{tag}"
                )))
                .respond_with(response)
                .expect(1)
                .mount(&server)
                .await;
        }
        let provider = NflverseData::in_memory().await.unwrap();
        // Configure before any handle is cloned or timer started.
        let mut provider = provider;
        Arc::get_mut(&mut provider.inner).unwrap().config = NflverseDataConfig {
            github_api_base: server.uri(),
            ..Default::default()
        };
        (provider, server)
    }

    #[tokio::test]
    async fn scheduler_tick_reports_started_busy_and_failed() {
        assert!(scheduler_tick(async { Ok(true) }).await);
        assert!(!scheduler_tick(async { Ok(false) }).await);
        assert!(
            !scheduler_tick(async {
                Err(NflverseDataError::Db(sqlx::Error::ColumnNotFound(
                    "tick".into(),
                )))
            })
            .await
        );
    }

    #[tokio::test(start_paused = true)]
    async fn timer_delays_first_tick_skips_late_ticks_and_stops() {
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        let task = tokio::spawn(run_scheduler_with(Duration::from_secs(10), move || {
            counter.fetch_add(1, Ordering::SeqCst);
            async { Ok(true) }
        }));
        tokio::task::yield_now().await;
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        tokio::time::advance(Duration::from_secs(9)).await;
        tokio::task::yield_now().await;
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        tokio::time::advance(Duration::from_secs(1)).await;
        tokio::task::yield_now().await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        tokio::time::advance(Duration::from_secs(35)).await;
        tokio::task::yield_now().await;
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        tokio::time::advance(Duration::from_secs(4)).await;
        tokio::task::yield_now().await;
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        tokio::time::advance(Duration::from_secs(1)).await;
        tokio::task::yield_now().await;
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        tokio::time::advance(Duration::from_secs(100)).await;
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn zero_disables_one_timer_is_shared_by_clones_and_drop_releases_it() {
        let provider = NflverseData::in_memory().await.unwrap();
        assert!(provider.inner.refresh.scheduler.lock().unwrap().is_none());
        assert!(!provider.start_scheduler(Duration::ZERO));
        assert!(!provider.stop_scheduler());
        assert!(provider.start_scheduler(Duration::from_secs(60)));
        let cloned = provider.clone();
        assert!(!cloned.start_scheduler(Duration::from_secs(30)));
        assert!(!cloned.start_scheduler(Duration::ZERO));
        assert!(!provider.stop_scheduler());
        assert!(cloned.start_scheduler(Duration::from_secs(60)));
        assert!(provider.stop_scheduler());
        assert!(!cloned.stop_scheduler());
        assert!(provider.start_scheduler(Duration::from_secs(60)));
        let weak = Arc::downgrade(&provider.inner);
        drop(cloned);
        drop(provider);
        assert!(weak.upgrade().is_none());
    }

    #[tokio::test]
    async fn manual_refresh_blocks_scheduled_refresh_and_stop_does_not_cancel_it() {
        let (provider, _server) = mock_provider(false, Duration::from_millis(30)).await;
        let cloned = provider.clone();
        let (first, second) = tokio::join!(provider.request_refresh(), cloned.request_refresh());
        assert_ne!(first.unwrap(), second.unwrap());
        match provider.refresh_status() {
            RunnerState::Running { progress, .. } => {
                assert_eq!(progress.len(), 6);
                assert!(progress.iter().all(|row| row.assets == 0));
            }
            other => panic!("expected running, got {other:?}"),
        }
        assert!(provider.start_scheduler(Duration::from_millis(10)));
        tokio::time::sleep(Duration::from_millis(25)).await;
        assert!(!provider.request_refresh().await.unwrap());
        assert!(provider.stop_scheduler());
        assert!(provider.inner.refresh.runner.is_running());
        let last = provider.inner.refresh.runner.wait_idle().await.unwrap();
        assert!(last.outcome.is_ok());
        assert!(matches!(
            provider.refresh_status(),
            RunnerState::Idle { last: Some(_) }
        ));
    }

    #[tokio::test]
    async fn scheduled_refresh_blocks_manual_refresh_and_publishes_failed_report() {
        let (provider, _server) = mock_provider(true, Duration::from_millis(30)).await;
        assert!(provider.start_scheduler(Duration::from_millis(10)));
        tokio::time::timeout(Duration::from_secs(2), async {
            while !provider.inner.refresh.runner.is_running() {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
        assert!(!provider.request_refresh().await.unwrap());
        provider.stop_scheduler();
        let last = provider.inner.refresh.runner.wait_idle().await.unwrap();
        assert!(last.outcome.unwrap_err().contains("FAILED"));
        assert!(matches!(
            provider.refresh_status(),
            RunnerState::Idle { last: Some(_) }
        ));
    }

    #[tokio::test]
    async fn awaited_sync_is_one_shot_and_does_not_publish_background_status() {
        let (provider, _server) = mock_provider(false, Duration::ZERO).await;
        assert!(provider.sync().await.unwrap().all_ok());
        assert!(matches!(
            provider.refresh_status(),
            RunnerState::Idle { last: None }
        ));
        assert!(!provider.stop_scheduler());
    }
}
