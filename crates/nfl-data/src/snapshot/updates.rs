//! The ordered update worker: one task turns sync events and local
//! invalidations into passes, reconciling slates before building a snapshot.

use std::collections::BTreeMap;
use std::sync::Arc;

use arc_swap::ArcSwap;
use nfl_model::Season;
use nflverse_data::{AssetUpdate, Dataset, SyncEvent};
use time::OffsetDateTime;
use tokio::sync::mpsc;

use crate::NflDataError;
use crate::slates::{self, Slate, SlateId};
use crate::snapshot::{Snapshot, SnapshotSources};
use crate::store::slates as slates_store;

/// The update worker's two input receivers, taken together once by
/// [`crate::NflData::start_background_tasks`].
pub(crate) struct UpdateInputs {
    pub(crate) sync_events: mpsc::UnboundedReceiver<SyncEvent>,
    pub(crate) snapshot_changes: mpsc::UnboundedReceiver<()>,
}

/// Work requested but not yet processed; a burst merges into one pass.
#[derive(Debug, Default)]
pub(crate) struct PendingWork {
    pub(crate) reconcile_slates: bool,
    pub(crate) rebuild_snapshot: bool,
}

impl PendingWork {
    /// The worker's startup state: reconcile the season and rebuild.
    pub(crate) fn startup() -> Self {
        Self {
            reconcile_slates: true,
            rebuild_snapshot: true,
        }
    }

    /// Classify a sync completion against the configured season: reconcile
    /// when the sync succeeded or a committed asset is relevant, rebuild when
    /// a relevant asset committed. A fully failed sync with no commits
    /// requests nothing.
    pub(crate) fn for_sync(event: &SyncEvent, season: Season) -> Self {
        let relevant = event
            .updates
            .iter()
            .any(|update| update_is_relevant(update, season));
        Self {
            reconcile_slates: event.all_ok || relevant,
            rebuild_snapshot: relevant,
        }
    }

    /// Keep everything pending and add the other request's work.
    pub(crate) fn merge(&mut self, other: Self) {
        self.reconcile_slates |= other.reconcile_slates;
        self.rebuild_snapshot |= other.rebuild_snapshot;
    }
}

/// Relevant: the update's season matches, or it is a shared schedules/players
/// update, whose single asset spans every season.
fn update_is_relevant(update: &AssetUpdate, season: Season) -> bool {
    update.season == Some(season) || matches!(update.dataset, Dataset::Schedules | Dataset::Players)
}

/// Run the update worker until both input channels close.
///
/// The worker owns its source clones and publication handle. Passes run one at
/// a time; arrivals during a pass stay queued for the next one, and none are
/// dropped.
pub(crate) async fn run(
    sources: SnapshotSources,
    published: Arc<ArcSwap<Snapshot>>,
    inputs: UpdateInputs,
) {
    let UpdateInputs {
        mut sync_events,
        mut snapshot_changes,
    } = inputs;
    let mut pending = PendingWork::startup();
    let mut sync_events_open = true;
    let mut snapshot_changes_open = true;

    loop {
        pending = process_pass(&sources, &published, pending).await;

        // A closed channel disables its branch until both are closed.
        loop {
            if !sync_events_open && !snapshot_changes_open {
                return;
            }
            tokio::select! {
                maybe = sync_events.recv(), if sync_events_open => match maybe {
                    Some(event) => {
                        pending.merge(PendingWork::for_sync(&event, sources.season));
                        break;
                    }
                    None => sync_events_open = false,
                },
                maybe = snapshot_changes.recv(), if snapshot_changes_open => match maybe {
                    Some(()) => {
                        pending.rebuild_snapshot = true;
                        break;
                    }
                    None => snapshot_changes_open = false,
                },
            }
        }

        // Absorb what is already queued so a burst combines into one pass.
        while let Ok(event) = sync_events.try_recv() {
            pending.merge(PendingWork::for_sync(&event, sources.season));
        }
        while snapshot_changes.try_recv().is_ok() {
            pending.rebuild_snapshot = true;
        }
    }
}

/// Reconcile when requested, then rebuild and publish. A failure retains the
/// work for the next notification.
async fn process_pass(
    sources: &SnapshotSources,
    published: &Arc<ArcSwap<Snapshot>>,
    mut pending: PendingWork,
) -> PendingWork {
    if pending.reconcile_slates {
        match reconcile_pass(sources).await {
            Ok(changed) => {
                if changed {
                    pending.rebuild_snapshot = true;
                }
            }
            Err(error) => {
                tracing::warn!(
                    %error,
                    "slate reconciliation failed; keeping the work for the next notification"
                );
                return PendingWork {
                    reconcile_slates: true,
                    rebuild_snapshot: true,
                };
            }
        }
        pending.reconcile_slates = false;
    }

    if pending.rebuild_snapshot {
        match Snapshot::build_from_sources(sources).await {
            Ok(snapshot) => published.store(Arc::new(snapshot)),
            Err(error) => {
                tracing::warn!(
                    %error,
                    "snapshot build failed; keeping the work for the next notification"
                );
                return PendingWork {
                    reconcile_slates: pending.reconcile_slates,
                    rebuild_snapshot: true,
                };
            }
        }
    }

    PendingWork::default()
}

/// Reconcile the season and report whether the stored slate state changed.
async fn reconcile_pass(sources: &SnapshotSources) -> Result<bool, NflDataError> {
    let before = stored_slates(sources).await?;
    slates::populate_at(
        &sources.provider,
        &sources.store,
        sources.season,
        OffsetDateTime::now_utc(),
    )
    .await?;
    let after = stored_slates(sources).await?;
    Ok(before != after)
}

/// The stored slate state a snapshot build can see.
async fn stored_slates(
    sources: &SnapshotSources,
) -> Result<BTreeMap<SlateId, (Slate, Vec<String>)>, NflDataError> {
    let mut state = BTreeMap::new();
    for slate in slates_store::for_season(sources.store.reader(), sources.season).await? {
        let game_ids = slates_store::game_ids(sources.store.reader(), &slate.id).await?;
        state.insert(slate.id, (slate, game_ids));
    }
    Ok(state)
}
