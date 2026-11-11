//! Scoring assembly a handler can call: score a fantasy team's entries from
//! the published snapshot's stored attachments.

use std::collections::HashSet;

use crate::entries::FantasyTeamEntry;
use nfl_data::slates::SlateId;

/// A lineup's total from the snapshot's stored attachments: the eight players
/// plus the defense. A pick with no salaried row or no attached week scores
/// nothing, so an inactive player costs a lineup exactly what it scored.
///
/// Eligibility is the caller's: this sums whatever the slate attached.
pub fn lineup_total(
    lineup: &crate::entries::Lineup,
    snapshot: &nfl_data::Snapshot,
    slate_id: SlateId,
) -> f64 {
    let players: f64 = lineup
        .player_ids()
        .iter()
        .filter_map(|id| snapshot.player(&slate_id, id))
        .filter_map(|player| player.scored_stats.as_ref())
        .map(|scored| scored.score.total)
        .sum();
    let defense = snapshot
        .defense(&slate_id, &lineup.def)
        .and_then(|player| player.scored_stats.as_ref())
        .map_or(0.0, |scored| scored.score.total);
    players + defense
}

/// Score one fantasy team's entries: each contest with an official slate gets
/// its stored total; the rest are skipped. Entry order is preserved.
pub fn score_entries(
    entries: &[FantasyTeamEntry],
    snapshot: &nfl_data::Snapshot,
    official_slates: &HashSet<SlateId>,
) -> Vec<(SlateId, f64)> {
    entries
        .iter()
        .filter(|entry| official_slates.contains(&entry.slate_id))
        .map(|entry| {
            (
                entry.slate_id,
                lineup_total(&entry.lineup, snapshot, entry.slate_id),
            )
        })
        .collect()
}
