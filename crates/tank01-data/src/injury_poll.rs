//! Tank01 injury polling lifecycle.
//!
//! One persistent worker fetches the source every five minutes while enabled.
//! The poller owns no generic scheduling machinery: [`crate::poll::SlatePoller`]
//! remains the live-game path.

use std::sync::Mutex;
use std::time::Duration;

use nfl_model::{InjuryReport, ProviderResponse};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::MissedTickBehavior;
use tracing::Instrument;

use crate::client::Tank01Client;

/// The source's injury list changes slowly; five minutes matches its refresh.
const INJURY_POLL_INTERVAL: Duration = Duration::from_secs(300);

/// Owns the single injury polling worker and its enable/disable signal.
pub struct InjuryPoller {
    client: Tank01Client,
    reports: mpsc::UnboundedSender<ProviderResponse<InjuryReport>>,
    enabled: watch::Sender<bool>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl Drop for InjuryPoller {
    fn drop(&mut self) {
        if let Some(worker) = self
            .worker
            .get_mut()
            .unwrap_or_else(|error| error.into_inner())
            .take()
        {
            // A detached worker would keep the client and sender alive.
            worker.abort();
        }
    }
}

impl InjuryPoller {
    /// Build the poller without starting any task.
    pub fn new(
        client: Tank01Client,
        reports: mpsc::UnboundedSender<ProviderResponse<InjuryReport>>,
    ) -> Self {
        // `send_replace` still records enablement before a worker exists.
        let (enabled, _) = watch::channel(false);
        Self {
            client,
            reports,
            enabled,
            worker: Mutex::new(None),
        }
    }

    /// Enable polling, spawning the worker on first use. Returns true only on a
    /// disabled-to-enabled transition; repeated starts are no-ops.
    pub fn start(&self) -> bool {
        let mut worker = self
            .worker
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let was_enabled = self.enabled.send_replace(true);
        if let Some(task) = worker.as_ref() {
            if !task.is_finished() {
                return !was_enabled;
            }
            if self.reports.is_closed() {
                // A finished worker restarted without a consumer would only
                // fetch to fail its send.
                self.enabled.send_replace(false);
                return false;
            }
        }
        *worker = Some(self.spawn_worker());
        !was_enabled
    }

    /// Disable future triggers. An active request and its send finish, and the
    /// worker stays alive for a later start.
    pub fn stop(&self) -> bool {
        let _worker = self
            .worker
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        self.enabled.send_replace(false)
    }

    fn spawn_worker(&self) -> JoinHandle<()> {
        let client = self.client.clone();
        let reports = self.reports.clone();
        let mut enabled = self.enabled.subscribe();
        // Root the span: the worker outlives any caller's request context.
        let span = tracing::info_span!(parent: None, "tank01_injury_poller");
        tokio::spawn(
            async move {
                let mut interval: Option<tokio::time::Interval> = None;
                loop {
                    if !*enabled.borrow_and_update() {
                        // Reenablement starts a fresh immediate-first interval.
                        interval = None;
                        tokio::select! {
                            changed = enabled.changed() => {
                                if changed.is_err() {
                                    return;
                                }
                            }
                            _ = reports.closed() => return,
                        }
                        continue;
                    }

                    let tick = interval.get_or_insert_with(|| {
                        let mut interval = tokio::time::interval(INJURY_POLL_INTERVAL);
                        interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
                        interval
                    });
                    tokio::select! {
                        _ = reports.closed() => return,
                        changed = enabled.changed() => {
                            if changed.is_err() {
                                return;
                            }
                            // A stop, or a stop followed by a start, is handled
                            // at the top of the loop before any fetch.
                            continue;
                        }
                        _ = tick.tick() => {}
                    }
                    if !*enabled.borrow() {
                        continue;
                    }

                    // Await the fetch and send outside the control select: a
                    // stop never cancels an in-flight request, and a restart
                    // during it never overlaps a second fetch.
                    let response = client.current_injuries().await;
                    if reports.send(response).is_err() {
                        return;
                    }
                }
            }
            .instrument(span),
        )
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use nfl_model::{ProviderOutcome, ProviderResponse};
    use tokio::sync::mpsc;
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
                .with_timeout(Duration::from_secs(60)),
        )
        .unwrap()
    }

    fn body() -> serde_json::Value {
        serde_json::json!({
            "statusCode": 200,
            "body": {
                "injList": [{
                    "team": "BUF",
                    "designation": "Q",
                    "pos": "QB",
                    "playerID": "3918298",
                    "longName": "Josh Allen"
                }],
                "injDate": "20260922"
            }
        })
    }

    async fn mount(server: &MockServer, delay: Duration) -> Arc<AtomicUsize> {
        let count = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&count);
        Mock::given(method("GET"))
            .and(path("/getNFLInjuriesByDate"))
            .respond_with(move |_request: &wiremock::Request| {
                observed.fetch_add(1, Ordering::SeqCst);
                ResponseTemplate::new(200)
                    .insert_header("content-type", "application/json")
                    .set_delay(delay)
                    .set_body_json(body())
            })
            .mount(server)
            .await;
        count
    }

    fn request_count(count: &AtomicUsize) -> usize {
        count.load(Ordering::SeqCst)
    }

    /// Wait until the mock server has seen `expected` requests, so a delayed
    /// response's timer is registered before the test advances paused time.
    /// Deliberately yield rather than await a channel: an idle runtime would
    /// auto-advance the paused clock and fire unrelated timers.
    async fn wait_for_requests(count: &AtomicUsize, expected: usize) {
        for _ in 0..200_000 {
            if request_count(count) >= expected {
                return;
            }
            tokio::task::yield_now().await;
        }
        panic!("mock server never saw request {expected}");
    }

    /// Keep the runtime busy so paused time does not auto-advance past a timer.
    async fn spin(ticks: usize) {
        for _ in 0..ticks {
            tokio::task::yield_now().await;
        }
    }

    async fn response(
        reports: &mut mpsc::UnboundedReceiver<ProviderResponse<InjuryReport>>,
    ) -> ProviderResponse<InjuryReport> {
        for _ in 0..200_000 {
            match reports.try_recv() {
                Ok(response) => return response,
                Err(mpsc::error::TryRecvError::Empty) => tokio::task::yield_now().await,
                Err(mpsc::error::TryRecvError::Disconnected) => panic!("report channel closed"),
            }
        }
        panic!("no injury response");
    }

    async fn assert_silent(reports: &mut mpsc::UnboundedReceiver<ProviderResponse<InjuryReport>>) {
        spin(5_000).await;
        assert!(
            matches!(
                reports.try_recv(),
                Err(mpsc::error::TryRecvError::Empty | mpsc::error::TryRecvError::Disconnected)
            ),
            "no injury report was expected"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn start_polls_immediately_and_every_five_minutes() {
        let server = MockServer::start().await;
        let count = mount(&server, Duration::ZERO).await;
        let (sender, mut reports) = mpsc::unbounded_channel();
        let poller = InjuryPoller::new(client(&server), sender);

        assert_eq!(request_count(&count), 0, "construction is inert");
        assert!(poller.start(), "first start, immediate request");
        response(&mut reports).await;
        assert_eq!(request_count(&count), 1);

        assert!(!poller.start(), "a repeated start is a no-op");
        tokio::time::advance(Duration::from_secs(299)).await;
        assert_silent(&mut reports).await;
        assert_eq!(request_count(&count), 1);

        tokio::time::advance(Duration::from_secs(1)).await;
        response(&mut reports).await;
        assert_eq!(request_count(&count), 2);

        assert!(poller.stop());
        tokio::time::advance(Duration::from_secs(600)).await;
        assert_silent(&mut reports).await;
        assert_eq!(request_count(&count), 2, "stopped adds no triggers");

        assert!(poller.start(), "reenabled from idle");
        response(&mut reports).await;
        assert_eq!(
            request_count(&count),
            3,
            "reenablement starts a fresh immediate-first interval"
        );
    }

    #[tokio::test]
    async fn stop_and_restart_during_a_fetch_never_overlap() {
        let server = MockServer::start().await;
        // The mock server runs its own runtime, so this delay is real time.
        let count = mount(&server, Duration::from_millis(300)).await;
        let (sender, mut reports) = mpsc::unbounded_channel();
        let poller = InjuryPoller::new(client(&server), sender);

        assert!(poller.start());
        wait_for_requests(&count, 1).await;

        assert!(poller.stop());
        assert!(poller.start(), "a stop followed by a start while fetching");
        let first = response(&mut reports).await;
        assert!(
            matches!(
                &first.outcome,
                ProviderOutcome::Value(report) if report.entries.len() == 1
            ),
            "unexpected outcome: {:?}",
            first.outcome
        );
        assert_eq!(
            request_count(&count),
            1,
            "the restart did not begin a parallel fetch"
        );

        // Restarting after the outstanding result begins a fresh fetch. Yield
        // first so the worker observes the stop while idle, not coalesced with
        // the start.
        assert!(poller.stop());
        spin(1_000).await;
        assert!(poller.start(), "restart resumes polling");
        wait_for_requests(&count, 2).await;
        response(&mut reports).await;
        assert_eq!(request_count(&count), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn a_closed_report_channel_terminates_and_blocks_restart() {
        let server = MockServer::start().await;
        let _count = mount(&server, Duration::ZERO).await;
        let (sender, reports) = mpsc::unbounded_channel();
        drop(reports);
        let poller = InjuryPoller::new(client(&server), sender);

        assert!(poller.start(), "the first start still transitions");
        spin(50_000).await;

        poller.stop();
        assert!(
            !poller.start(),
            "a closed channel must not spawn another worker"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn drop_aborts_the_worker() {
        let server = MockServer::start().await;
        let count = mount(&server, Duration::ZERO).await;
        let (sender, mut reports) = mpsc::unbounded_channel();
        let poller = InjuryPoller::new(client(&server), sender);
        assert!(poller.start());
        response(&mut reports).await;

        drop(poller);
        tokio::time::advance(Duration::from_secs(300)).await;
        assert_silent(&mut reports).await;
        assert_eq!(request_count(&count), 1);
    }
}
