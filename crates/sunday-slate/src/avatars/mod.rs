mod fetch;

use std::collections::HashMap;

use crate::media::Media;
use crate::nfl_players::store as player_store;
use crate::nfl_teams::store as team_store;
use crate::{AppError, AppState, Db};
use std::collections::HashSet;

pub use fetch::{
    AvatarFetchRunner, FetchProgress, FetchReport, espn_logo_url, fetch_missing, start_fetch,
};

#[derive(Debug, Default)]
pub struct Avatars {
    pub players: HashMap<String, Media>,
    pub teams: HashMap<String, Media>,
}

pub async fn for_lineup(
    db: &Db,
    player_ids: &[&str],
    team_abbrs: &[&str],
) -> Result<Avatars, AppError> {
    let players = player_store::headshots_for(db.reader(), player_ids).await?;
    let teams = team_store::logos_for(db.reader(), team_abbrs).await?;
    Ok(Avatars { players, teams })
}

/// GSIS ids that should get a headshot: reconciled identities from nfl-data
/// plus local rows without one, minus ids already carrying a headshot.
/// Deduplicated and sorted.
pub(crate) async fn missing_player_headshot_ids(state: &AppState) -> Result<Vec<String>, AppError> {
    let reconciled = state.nfl.reconciled_gsis_ids().await?;
    let local = player_store::headshot_player_ids(state.db.reader()).await?;
    let with_headshot: HashSet<String> = player_store::headshot_ids_with_media(state.db.reader())
        .await?
        .into_iter()
        .collect();

    let mut candidates: HashSet<String> = reconciled.into_iter().collect();
    candidates.extend(local);
    for id in with_headshot {
        candidates.remove(&id);
    }
    let mut out: Vec<String> = candidates.into_iter().collect();
    out.sort();
    Ok(out)
}
