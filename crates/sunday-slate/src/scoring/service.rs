use crate::entries::Lineup;
use nfl_data::Snapshot;
use std::collections::HashSet;

use crate::entries::FantasyTeamEntry;
use nfl_data::slates::SlateId;

/// Does not check official eligibility; callers must select eligible slates.
pub fn lineup_total(lineup: &Lineup, snapshot: &Snapshot, slate_id: SlateId) -> f64 {
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

pub fn score_entries(
    entries: &[FantasyTeamEntry],
    snapshot: &Snapshot,
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
