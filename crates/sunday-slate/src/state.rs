use nfl_data::NflData;
use std::sync::Arc;
use tower_sessions_sqlx_store::SqliteStore;

use crate::avatars::AvatarFetchRunner;
use crate::{Config, Db, mail::Mailer};

#[derive(Clone)]
pub struct AppState {
    pub db: Db,
    pub sessions: SqliteStore,
    pub config: Arc<Config>,
    pub mailer: Mailer,
    pub nfl: Arc<NflData>,
    pub avatar_runner: Arc<AvatarFetchRunner>,
    pub live: Arc<crate::live::LiveContestHub>,
    pub injuries: Arc<crate::injuries::InjuryReports>,
}

impl AppState {
    pub fn now(&self) -> time::OffsetDateTime {
        self.config.now()
    }

    /// `now()` in US Eastern, so day boundaries match slate dates.
    pub fn now_eastern(&self) -> time::OffsetDateTime {
        nfl_data::to_eastern(self.now())
    }
}
