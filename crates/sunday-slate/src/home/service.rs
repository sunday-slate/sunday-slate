//! Resolves the Current Contest from the published snapshot.

use nfl_data::slates::SlateId;

use crate::AppState;

/// The Current Contest: the snapshot's authoritative current slate.
/// `None` when no slate is published.
pub fn current_contest_id(state: &AppState) -> Option<SlateId> {
    state.nfl.snapshot().current_slate().map(|slate| slate.id)
}
