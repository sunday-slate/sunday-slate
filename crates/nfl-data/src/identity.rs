use nfl_model::{NflPlayerId, TeamAbbr};
use nflverse_data::NflverseData;
use std::collections::HashMap;

use crate::NflDataError;

/// How a page names one pick: the display facts every consumer of a gsis id
/// needs, resolved from whichever source carries the player.
#[derive(Debug, Clone, PartialEq)]
pub struct PlayerIdentity {
    pub name: String,
    pub team: Option<TeamAbbr>,
    pub position: Option<String>,
    /// Player photo URL from the `players` release. `None` for picks named
    /// only from weekly rosters (that source carries no headshot).
    pub headshot_url: Option<String>,
}

/// Display facts per gsis id from the provider: `players` first, weekly rosters
/// for the misses; unknown ids are absent from the map.
pub(crate) async fn identify(
    provider: &NflverseData,
    ids: &[NflPlayerId],
) -> Result<HashMap<NflPlayerId, PlayerIdentity>, NflDataError> {
    let players = provider.players_by_gsis(ids).await?;
    let unnamed: Vec<NflPlayerId> = ids
        .iter()
        .filter(|id| !players.contains_key(id.0.as_str()))
        .cloned()
        .collect();
    let roster = provider.weekly_roster_entries_by_gsis(&unnamed).await?;
    let named = players.into_iter().map(|(id, p)| {
        let identity = PlayerIdentity {
            name: p.full_name,
            team: p.latest_team,
            position: p.position,
            headshot_url: p.headshot_url,
        };
        (NflPlayerId(id), identity)
    });
    let rostered = roster.into_iter().map(|(id, e)| {
        let identity = PlayerIdentity {
            name: e.full_name,
            team: Some(e.team),
            position: e.position,
            headshot_url: None,
        };
        (NflPlayerId(id), identity)
    });
    Ok(named.chain(rostered).collect())
}
