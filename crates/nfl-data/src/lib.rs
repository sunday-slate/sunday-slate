mod config;
mod data_revision;
mod error;
mod identity;
mod nflverse;
pub mod salaried_players;
mod salaries;
pub mod scoring;
pub mod slates;
pub mod snapshot;
mod store;
mod sync;
#[cfg(test)]
mod test;
mod web;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use arc_swap::ArcSwap;
use nflverse_data::{NflverseData, SyncEvent};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::Instrument;

pub use config::NflDataConfig;
pub use data_revision::DataRevision;
pub use error::NflDataError;
pub use identity::PlayerIdentity;
pub use nfl_model::live::{
    EspnPlayerId, InjuryDesignation, InjuryEntry, InjuryProvider, LiveGame, LiveGamePhase,
    LiveGameSnapshot, LivePlayerStats, LiveProviderError, LiveScoreProvider, LiveScoreboard,
    LiveSlate, LiveTeamStats, ProviderOutcome, ProviderResponse,
};
pub use nfl_model::{
    DfsPosition, Game, Player, PlayerWeekStats, Season, SeasonType, SeasonWeek, TeamAbbr,
    TeamWeekStats, Week, WeeklyRosterEntry, eastern_offset, to_eastern,
};
pub(crate) use salaries::SalaryWeekOverview;
pub use snapshot::Snapshot;
pub(crate) use store::PlayerLink;
pub use web::AdminConfig;
pub use web::router as admin_router;

pub struct NflData {
    /// The published snapshot; the shared handle lets a rebuild swap it while
    /// readers keep the snapshot they took.
    pub(crate) snapshot: ArcSwap<Snapshot>,
    pub(crate) nflverse: NflverseData,
    pub(crate) fanduel: fanduel_data::FanduelData,
    pub(crate) store: store::Store,
    pub(crate) refresh_interval: Option<Duration>,
    pub(crate) season: Season,
    /// Serializes slate reconciliation, builds, and publication.
    pub(crate) publication: tokio::sync::Mutex<()>,
    /// Receive end of the nflverse source's sync events, taken once by
    /// [`Self::start_background_tasks`].
    pub(crate) sync_events: Mutex<Option<mpsc::UnboundedReceiver<SyncEvent>>>,
    /// The notification task; aborted on drop.
    pub(crate) update_worker: Mutex<Option<JoinHandle<()>>>,
}

impl Drop for NflData {
    fn drop(&mut self) {
        if let Some(worker) = self
            .update_worker
            .get_mut()
            .unwrap_or_else(|error| error.into_inner())
            .take()
        {
            // Dropping a JoinHandle only detaches; the worker holds source
            // clones and would outlive the handle.
            worker.abort();
        }
    }
}

impl NflData {
    /// Opens all databases, runs their migrations, reconciles the configured
    /// season's slates, and builds the initial snapshot. Any failure is a
    /// connection error; background work starts only through
    /// [`Self::start_background_tasks`].
    pub async fn connect(config: NflDataConfig) -> Result<Self, NflDataError> {
        let refresh_interval = config
            .refresh_interval
            .filter(|interval| !interval.is_zero());
        let (sync_events, sync_events_receiver) = mpsc::unbounded_channel();
        let nflverse =
            NflverseData::connect_with_sync_events(config.clone().into(), Some(sync_events))
                .await?;
        let fanduel = if config.fanduel_database_url == "sqlite::memory:" {
            fanduel_data::FanduelData::in_memory().await
        } else {
            fanduel_data::FanduelData::connect(&config.fanduel_database_url).await
        }?;
        let store = store::Store::open(&config.database_url).await?;

        let nfl_data = Self {
            nflverse,
            fanduel,
            store,
            refresh_interval,
            season: config.season,
            snapshot: ArcSwap::from_pointee(Snapshot::default()),
            publication: tokio::sync::Mutex::new(()),
            sync_events: Mutex::new(Some(sync_events_receiver)),
            update_worker: Mutex::new(None),
        };

        // The initial build reads stored slates, so reconcile them first.
        let initial = async {
            slates::refresh(&nfl_data).await?;
            nfl_data.build_and_publish().await
        };
        initial
            .instrument(tracing::info_span!(
                parent: None,
                "snapshot_update",
                season = nfl_data.season.0,
                trigger = "initial_connect",
            ))
            .await?;

        Ok(nfl_data)
    }

    /// A fresh, empty, in-memory cache: migrations applied, nothing synced.
    /// Nothing builds a snapshot behind the caller's back: seeding tests
    /// republish explicitly.
    pub async fn in_memory() -> Result<Self, NflDataError> {
        let nflverse = NflverseData::in_memory().await?;
        let fanduel = fanduel_data::FanduelData::in_memory().await?;
        let store = store::Store::in_memory().await?;
        Ok(Self {
            nflverse,
            fanduel,
            store,
            refresh_interval: None,
            season: Season::default(),
            snapshot: ArcSwap::from_pointee(Snapshot::default()),
            publication: tokio::sync::Mutex::new(()),
            sync_events: Mutex::new(None),
            update_worker: Mutex::new(None),
        })
    }

    /// The application season this handle carries, from its configuration.
    pub fn season(&self) -> Season {
        self.season
    }

    pub fn snapshot(&self) -> Arc<Snapshot> {
        self.snapshot.load_full()
    }
}
