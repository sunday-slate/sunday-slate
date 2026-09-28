mod fetch;

use std::collections::HashMap;

use crate::media::Media;
use crate::nfl_players::store as player_store;
use crate::nfl_teams::store as team_store;
use crate::{AppError, Db};

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
