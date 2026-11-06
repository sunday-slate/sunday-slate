//! Background refresh lifecycle and post-sync reconciliation.

use std::collections::BTreeMap;
use std::sync::{Arc, Weak};

use nflverse_data::{Dataset, SyncEvent};

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
            self.provider.start_scheduler(interval);
        }
        self.start_update_worker();
    }

    /// Stop future automatic task triggers without cancelling an active
    /// refresh. The notification task keeps running: aborting it here would
    /// make a later start unable to restore its receiver.
    pub fn stop_background_tasks(&self) {
        self.provider.stop_scheduler();
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

    /// Apply one completed sync: republish when the configured season's data
    /// changed, otherwise reconcile the slates only.
    async fn apply_sync_event(&self, event: SyncEvent) {
        let relevant = event
            .updates
            .iter()
            .any(|update| update_is_relevant(update, self.season));
        let outcome = if relevant {
            // A committed relevant asset rebuilds even when a sibling dataset
            // failed: its rows are in the store either way.
            self.reconcile_slates_and_rebuild().await
        } else if event.all_ok {
            // An unchanged or historical-only sync still reconciles the
            // slates, and republishes only when that changed what a build
            // reads.
            self.reconcile_slates_if_changed().await
        } else {
            // Nothing committed that this season can see.
            return;
        };
        if let Err(error) = outcome {
            tracing::warn!(%error, "snapshot update after sync failed");
        }
    }

    /// Reconcile the configured season's slates and republish. Returns after
    /// the new snapshot is published, or leaves the old one on error.
    pub(crate) async fn reconcile_slates_and_rebuild(&self) -> Result<(), NflDataError> {
        let _publication = self.publication.lock().await;
        slates::refresh(self).await?;
        self.build_and_publish().await
    }

    /// Reconcile the slates, republishing only when their stored state changed.
    async fn reconcile_slates_if_changed(&self) -> Result<(), NflDataError> {
        let _publication = self.publication.lock().await;
        let before = stored_slates(self).await?;
        slates::refresh(self).await?;
        if stored_slates(self).await? == before {
            return Ok(());
        }
        self.build_and_publish().await
    }

    /// Rebuild the snapshot and publish it. A failed build keeps the snapshot
    /// readers already hold; the next request to rebuild tries again.
    pub(crate) async fn rebuild_snapshot(&self) -> Result<(), NflDataError> {
        let _publication = self.publication.lock().await;
        self.build_and_publish().await
    }

    /// [`Self::rebuild_snapshot`] with the publication lock already held.
    pub(crate) async fn build_and_publish(&self) -> Result<(), NflDataError> {
        let snapshot = Snapshot::build(self).await?;
        self.snapshot.store(Arc::new(snapshot));
        Ok(())
    }
}
