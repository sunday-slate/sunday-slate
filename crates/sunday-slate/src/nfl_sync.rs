use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use nfl_data::{DatasetFreshness, NflData, NflDataError};
use tokio::time::MissedTickBehavior;

use crate::background::Runner;

/// Progress is the dataset freshness captured at start. The outcome is the
/// report text: `Ok` when every dataset synced, `Err` otherwise.
pub type NflverseSyncRunner = Runner<Vec<DatasetFreshness>, String>;

/// Start a sync unless one is already running.
pub async fn start_nfl_sync(
    runner: &Arc<NflverseSyncRunner>,
    nfl: Arc<NflData>,
) -> Result<bool, NflDataError> {
    if runner.is_running() {
        return Ok(false);
    }
    let freshness = nfl.freshness().await?;
    Ok(runner.try_start(freshness, move |_| async move {
        match nfl.sync().await {
            Ok(report) if report.all_ok() => Ok(report.to_string()),
            Ok(report) => Err(report.to_string()),
            Err(error) => Err(error.to_string()),
        }
    }))
}

/// One scheduled tick: logs a busy runner or a failed start and reports
/// whether a new sync began.
pub async fn scheduler_tick(attempt: impl Future<Output = Result<bool, NflDataError>>) -> bool {
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

/// Run `attempt` every `interval` until the task is aborted. The generic
/// inner is unit-tested; `run_scheduler` is the thin production wrapper.
pub async fn run_scheduler_with<F, Fut>(interval: Duration, mut attempt: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<bool, NflDataError>> + Send,
{
    let mut ticker = tokio::time::interval(interval);
    // The syncs run detached (Runner::try_start), so they cannot stall this
    // loop past a deadline; Skip just re-aligns late polls — task starvation —
    // to the next due tick instead of a catch-up burst.
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    // tokio's interval fires its first tick immediately; consume it so the
    // first sync runs a full interval after boot, with no catch-up for
    // downtime.
    let _ = ticker.tick().await;
    loop {
        ticker.tick().await;
        scheduler_tick(attempt()).await;
    }
}

/// Refresh the nflverse cache every `interval` until aborted, through the
/// same `start_nfl_sync` the admin button calls.
pub async fn run_scheduler(nfl: Arc<NflData>, runner: Arc<NflverseSyncRunner>, interval: Duration) {
    run_scheduler_with(interval, || {
        let nfl = Arc::clone(&nfl);
        let runner = Arc::clone(&runner);
        async move { start_nfl_sync(&runner, nfl).await }
    })
    .await
}

/// Human cadence for the sync interval, e.g. `every hour`.
pub fn interval_label(secs: u64) -> String {
    let (n, unit) = if secs.is_multiple_of(3600) {
        (secs / 3600, "hour")
    } else if secs.is_multiple_of(60) {
        (secs / 60, "minute")
    } else {
        return format!("every {secs} seconds");
    };
    if n == 1 {
        format!("every {unit}")
    } else {
        format!("every {n} {unit}s")
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{interval_label, run_scheduler_with, scheduler_tick};
    use nfl_data::NflDataError;

    #[tokio::test]
    async fn scheduler_tick_reports_whether_a_sync_started() {
        assert!(scheduler_tick(async { Ok(true) }).await);
        assert!(!scheduler_tick(async { Ok(false) }).await);
        assert!(
            !scheduler_tick(async {
                Err::<bool, _>(NflDataError::Db(sqlx::Error::ColumnNotFound("tick".into())))
            })
            .await
        );
    }

    #[tokio::test(start_paused = true)]
    async fn run_scheduler_ticks_on_interval_then_stops_when_aborted() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let handle = tokio::spawn(run_scheduler_with(Duration::from_millis(20), move || {
            let tx = tx.clone();
            async move {
                tx.send(()).expect("receiver alive");
                Ok::<bool, NflDataError>(true)
            }
        }));
        for _ in 0..3 {
            tokio::time::timeout(Duration::from_secs(2), rx.recv())
                .await
                .expect("tick within timeout")
                .expect("channel open");
        }
        handle.abort();
        assert!(
            handle
                .await
                .expect_err("aborted scheduler yields JoinError")
                .is_cancelled()
        );
    }

    #[test]
    fn interval_label_formats_hours_minutes_and_seconds() {
        assert_eq!(interval_label(3600), "every hour");
        assert_eq!(interval_label(7200), "every 2 hours");
        assert_eq!(interval_label(5400), "every 90 minutes");
        assert_eq!(interval_label(60), "every minute");
        assert_eq!(interval_label(900), "every 15 minutes");
        assert_eq!(interval_label(45), "every 45 seconds");
    }
}
