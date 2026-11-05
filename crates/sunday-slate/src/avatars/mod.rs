mod fetch;

use std::collections::HashMap;

use crate::entries::PlayerOrDefenseId;
use crate::media::Media;
use crate::nfl_player_headshots::store as player_store;
use crate::nfl_teams::store as team_store;
use crate::{AppError, Db};

pub use fetch::{
    AvatarFetchRunner, FetchProgress, FetchReport, espn_logo_url, fetch_missing, start_fetch,
};

/// The local headshot or logo for each pick, keyed by what the slot holds.
pub type Avatars = HashMap<PlayerOrDefenseId, Media>;

/// Load the local media for every given id. Players fetch headshots, defenses
/// fetch logos; an id with no cached media is absent from the map.
pub async fn by_id(db: &Db, values: &[PlayerOrDefenseId]) -> Result<Avatars, AppError> {
    let mut player_ids = Vec::new();
    let mut teams = Vec::new();
    for value in values {
        match value {
            PlayerOrDefenseId::Player(id) => player_ids.push(id.as_ref()),
            PlayerOrDefenseId::Defense(team) => teams.push(team.as_str()),
        }
    }

    let mut headshots = player_store::headshots_for(db.reader(), &player_ids).await?;
    let mut logos = team_store::logos_for(db.reader(), &teams).await?;

    let mut avatars = HashMap::new();
    for value in values {
        let media = match value {
            PlayerOrDefenseId::Player(id) => headshots.remove(id.as_ref()),
            PlayerOrDefenseId::Defense(team) => logos.remove(team.as_str()),
        };
        if let Some(media) = media {
            avatars.insert(value.clone(), media);
        }
    }
    Ok(avatars)
}
