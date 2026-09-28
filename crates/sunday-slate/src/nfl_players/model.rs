use crate::media::MediaId;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct NflPlayer {
    pub id: i64,
    pub gsis_player_id: String,
    pub fd_player_id: Option<String>,
    pub headshot_media_id: Option<MediaId>,
}
