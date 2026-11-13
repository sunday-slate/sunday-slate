//! Background refresh lifecycle and post-sync reconciliation.

use std::collections::BTreeMap;
use std::sync::{Arc, Weak};
use std::time::Instant;

use nflverse_data::{Dataset, SyncEvent};
use tracing::Instrument;

use crate::slates::{self, Slate, SlateId};
use crate::snapshot::Snapshot;
use crate::store;
use crate::{NflData, NflDataError, Season};

/// Identity fallback can read weekly rosters from any season.
fn update_is_relevant(update: &nflverse_data::AssetUpdate, season: Season) -> bool {
    update.season == Some(season)
        || matches!(
            update.dataset,
            Dataset::Schedules | Dataset::Players | Dataset::WeeklyRosters
        )
}

/// The stored slate state a build reads: metadata plus game membership.
async fn stored_slates(
    nfl: &NflData,
) -> Result<BTreeMap<SlateId, (Slate, Vec<String>)>, NflDataError> {
    let mut state = BTreeMap::new();
    for slate in slates::all(nfl).await? {
        let game_ids = store::slates::game_ids(nfl.store.reader(), &slate.id).await?;
        state.insert(slate.id, (slate, game_ids));
    }
    Ok(state)
}

impl NflData {
    /// Start configured internal background work. Repeated calls are safe: the
    /// scheduler refreshes upstream datasets when configured, and the
    /// notification task republishes the snapshot after each relevant sync.
    pub fn start_background_tasks(self: &Arc<Self>) {
        if let Some(interval) = self.refresh_interval {
            self.nflverse.start_scheduler(interval);
        }
        self.start_update_worker();
        // The consumer must exist before the first fetched report arrives.
        self.start_injury_worker();
        self.start_injury_polling();
    }

    /// Stop future automatic task triggers without cancelling an active
    /// refresh. The notification task keeps running: aborting it here would
    /// make a later start unable to restore its receiver.
    pub fn stop_background_tasks(&self) {
        self.nflverse.stop_scheduler();
        // An active injury fetch still delivers to the consumer.
        self.stop_injury_polling();
    }

    /// Spawn the notification task; the receiver is taken on the first call, so
    /// later calls are no-ops.
    fn start_update_worker(self: &Arc<Self>) {
        let Some(mut sync_events) = self
            .sync_events
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
        else {
            return;
        };
        // A weak owner keeps a dropped handle droppable: the task upgrades
        // only to process one event and releases it before waiting again.
        let updates = Arc::downgrade(self);
        let worker = tokio::spawn(async move {
            while let Some(event) = sync_events.recv().await {
                let Some(nfl) = Weak::upgrade(&updates) else {
                    return;
                };
                nfl.apply_sync_event(event).await;
            }
        });
        *self
            .update_worker
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(worker);
    }

    /// Span for an update the caller owns; nested work inherits its trigger.
    fn snapshot_update_span(&self, trigger: &'static str) -> tracing::Span {
        tracing::info_span!("snapshot_update", season = self.season.0, trigger)
    }

    /// A failed operation leaves readers on the current snapshot.
    fn log_update_failure(&self, error: &NflDataError) {
        tracing::error!(error = %error, "snapshot update failed");
    }

    /// Apply one completed sync: republish when the configured season's data
    /// changed, otherwise reconcile the slates only.
    async fn apply_sync_event(&self, event: SyncEvent) {
        let relevant = event
            .updates
            .iter()
            .any(|update| update_is_relevant(update, self.season));
        // Channels carry no request context: root the span explicitly.
        let span = tracing::info_span!(
            parent: None,
            "snapshot_update",
            season = self.season.0,
            trigger = "sync_event",
            committed_updates = event.updates.len(),
            relevant,
        );
        async move {
            if !relevant && !event.all_ok {
                // Nothing committed that this season can see.
                tracing::debug!("snapshot update skipped");
                return;
            }
            tracing::info!("snapshot update started");
            let outcome = if relevant {
                self.reconcile_slates_and_rebuild_inner().await
            } else {
                self.reconcile_slates_if_changed_inner().await
            };
            // Failures were logged at the operation that failed.
            drop(outcome);
        }
        .instrument(span)
        .await
    }

    /// Reconcile the configured season's slates and republish. Returns after
    /// the new snapshot is published, or leaves the old one on error.
    pub(crate) async fn reconcile_slates_and_rebuild(&self) -> Result<(), NflDataError> {
        let span = self.snapshot_update_span("reconcile_slates");
        async {
            tracing::info!("snapshot update started");
            self.reconcile_slates_and_rebuild_inner().await
        }
        .instrument(span)
        .await
    }

    /// [`Self::reconcile_slates_and_rebuild`] without a start event.
    async fn reconcile_slates_and_rebuild_inner(&self) -> Result<(), NflDataError> {
        let _publication = self.publication.lock().await;
        if let Err(error) = slates::refresh(self).await {
            self.log_update_failure(&error);
            return Err(error);
        }
        self.build_and_publish().await
    }

    /// Reconcile and republish only when stored slate state changed.
    async fn reconcile_slates_if_changed_inner(&self) -> Result<(), NflDataError> {
        let _publication = self.publication.lock().await;
        let before = match stored_slates(self).await {
            Ok(before) => before,
            Err(error) => {
                self.log_update_failure(&error);
                return Err(error);
            }
        };
        if let Err(error) = slates::refresh(self).await {
            self.log_update_failure(&error);
            return Err(error);
        }
        let after = match stored_slates(self).await {
            Ok(after) => after,
            Err(error) => {
                self.log_update_failure(&error);
                return Err(error);
            }
        };
        if after == before {
            tracing::debug!("snapshot update skipped");
            return Ok(());
        }
        self.build_and_publish().await
    }

    /// Rebuild the snapshot and publish it. A failed build keeps the snapshot
    /// readers already hold; the next request to rebuild tries again.
    pub(crate) async fn rebuild_snapshot(&self) -> Result<(), NflDataError> {
        let span = self.snapshot_update_span("rebuild");
        async {
            tracing::info!("snapshot update started");
            let _publication = self.publication.lock().await;
            self.build_and_publish().await
        }
        .instrument(span)
        .await
    }

    /// [`Self::rebuild_snapshot`] with the publication lock already held.
    pub(crate) async fn build_and_publish(&self) -> Result<(), NflDataError> {
        let started = Instant::now();
        let snapshot = match Snapshot::build(self).await {
            Ok(snapshot) => snapshot,
            Err(error) => {
                tracing::error!(
                    error = %error,
                    duration_ms = started.elapsed().as_millis() as u64,
                    "snapshot update failed"
                );
                return Err(error);
            }
        };
        // The atomic store is the publication boundary.
        self.snapshot.store(Arc::new(snapshot));
        tracing::info!(
            duration_ms = started.elapsed().as_millis() as u64,
            "snapshot published"
        );
        Ok(())
    }
}
