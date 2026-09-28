use crate::media::MediaId;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct NflTeam {
    pub abbr: String,
    pub logo_media_id: Option<MediaId>,
}
