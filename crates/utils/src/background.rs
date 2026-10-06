//! A one-at-a-time background job with a pollable state.
//!
//! `Runner<P, R>` holds `Idle` or `Running` in a mutex. `try_start` spawns
//! the job and refuses a second start while one runs. The job publishes
//! progress through `Progress::update`. A drop guard returns the runner to
//! `Idle` when the job ends, even on panic. The lock is never held across
//! an await.

use std::future::Future;
use std::sync::{Arc, Mutex, MutexGuard};

use time::OffsetDateTime;

pub struct Runner<P, R> {
    state: Mutex<RunnerState<P, R>>,
}

#[derive(Debug, Clone)]
pub enum RunnerState<P, R> {
    Idle {
        last: Option<LastRun<R>>,
    },
    Running {
        started_at: OffsetDateTime,
        progress: P,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LastRun<R> {
    pub finished_at: OffsetDateTime,
    pub outcome: Result<R, String>,
}

/// Handle the job uses to publish progress.
pub struct Progress<P, R> {
    runner: Arc<Runner<P, R>>,
}

impl<P, R> Progress<P, R> {
    /// Lock, mutate, release. No await runs under the lock.
    pub fn update(&self, f: impl FnOnce(&mut P)) {
        if let RunnerState::Running { progress, .. } = &mut *self.runner.lock() {
            f(progress);
        }
    }

    /// Build a `Progress` handle bound to `runner` outside of `try_start`,
    /// for tests that need to drive `fetch_missing_with` directly against a
    /// runner already placed in `Running`.
    #[cfg(any(test, feature = "test-support"))]
    pub fn for_test(runner: Arc<Runner<P, R>>) -> Self {
        Self { runner }
    }
}

impl<P, R> Default for Runner<P, R> {
    fn default() -> Self {
        Self::new()
    }
}

impl<P, R> Runner<P, R> {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(RunnerState::Idle { last: None }),
        }
    }

    fn lock(&self) -> MutexGuard<'_, RunnerState<P, R>> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn is_running(&self) -> bool {
        matches!(*self.lock(), RunnerState::Running { .. })
    }
}

impl<P: Clone + Send + 'static, R: Clone + Send + 'static> Runner<P, R> {
    /// Start `job` unless a run is in progress. Returns `false` when one is.
    pub fn try_start<F, Fut>(self: &Arc<Self>, initial: P, job: F) -> bool
    where
        F: FnOnce(Progress<P, R>) -> Fut + Send + 'static,
        Fut: Future<Output = Result<R, String>> + Send + 'static,
    {
        {
            let mut state = self.lock();
            if matches!(*state, RunnerState::Running { .. }) {
                return false;
            }
            *state = RunnerState::Running {
                started_at: OffsetDateTime::now_utc(),
                progress: initial,
            };
        }
        let mut guard = RunningGuard {
            runner: Arc::clone(self),
            outcome: None,
        };
        let progress = Progress {
            runner: Arc::clone(self),
        };
        tokio::spawn(async move {
            guard.outcome = Some(job(progress).await);
            // Explicit: the guard's Drop publishes the outcome.
            drop(guard);
        });
        true
    }

    pub fn snapshot(&self) -> RunnerState<P, R> {
        self.lock().clone()
    }

    /// Poll until the runner is `Idle` and return its last run. Panics after
    /// two seconds.
    #[cfg(any(test, feature = "test-support"))]
    pub async fn wait_idle(&self) -> Option<LastRun<R>> {
        for _ in 0..400 {
            if let RunnerState::Idle { last } = self.snapshot() {
                return last;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        panic!("runner did not go idle");
    }
}

/// Resets the runner to `Idle` when the job ends, whether it returned or
/// panicked.
struct RunningGuard<P, R> {
    runner: Arc<Runner<P, R>>,
    outcome: Option<Result<R, String>>,
}

impl<P, R> Drop for RunningGuard<P, R> {
    fn drop(&mut self) {
        let outcome = self
            .outcome
            .take()
            .unwrap_or_else(|| Err("interrupted before completion".to_string()));
        *self.runner.lock() = RunnerState::Idle {
            last: Some(LastRun {
                finished_at: OffsetDateTime::now_utc(),
                outcome,
            }),
        };
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use super::*;

    #[tokio::test]
    async fn second_start_is_refused_while_running() {
        let runner: Arc<Runner<u32, u32>> = Arc::new(Runner::new());
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        assert!(runner.try_start(0, |_| async move {
            let _ = rx.await;
            Ok(1)
        }));
        assert!(!runner.try_start(0, |_| async { Ok(2) }));
        assert!(runner.is_running());
        tx.send(()).unwrap();
        let last = runner.wait_idle().await.expect("last run");
        assert_eq!(last.outcome, Ok(1));
    }

    #[tokio::test]
    async fn progress_updates_are_visible_in_snapshot() {
        let runner: Arc<Runner<u32, ()>> = Arc::new(Runner::new());
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        runner.try_start(0, |progress| async move {
            progress.update(|p| *p = 7);
            let _ = rx.await;
            Ok(())
        });
        for _ in 0..200 {
            if let RunnerState::Running { progress: 7, .. } = runner.snapshot() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(matches!(
            runner.snapshot(),
            RunnerState::Running { progress: 7, .. }
        ));
        tx.send(()).unwrap();
        runner.wait_idle().await;
    }

    #[tokio::test]
    async fn error_outcome_is_kept() {
        let runner: Arc<Runner<(), ()>> = Arc::new(Runner::new());
        runner.try_start((), |_| async { Err("boom".to_string()) });
        let last = runner.wait_idle().await.expect("last run");
        assert_eq!(last.outcome, Err("boom".to_string()));
    }

    #[tokio::test]
    async fn panic_leaves_idle_with_interrupted_message() {
        let runner: Arc<Runner<(), ()>> = Arc::new(Runner::new());
        runner.try_start((), |_| async { panic!("job died") });
        let last = runner.wait_idle().await.expect("last run");
        assert_eq!(
            last.outcome,
            Err("interrupted before completion".to_string())
        );
    }
}
