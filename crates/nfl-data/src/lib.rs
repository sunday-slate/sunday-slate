mod config;
mod data_revision;
mod error;
mod identity;
pub mod salaried_players;
mod salaries;
pub mod scoring;
pub mod slates;
pub mod snapshot;
mod store;
#[cfg(test)]
mod test;
mod web;

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, Weak};

use arc_swap::ArcSwap;
use nflverse_data::{NflverseData, SyncEvent};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::slates::{Slate, SlateId};
pub use crate::snapshot::Snapshot;
pub use config::NflDataConfig;
pub use data_revision::DataRevision;
pub use error::NflDataError;
pub use fanduel_data::{FanduelDataError, SalariedPlayer};
pub use identity::PlayerIdentity;
pub use live::{
    EspnPlayerId, InjuryDesignation, InjuryEntry, InjuryProvider, InjuryReport, LiveGame,
    LiveGamePhase, LiveGameSnapshot, LivePlayerStats, LiveProviderError, LiveScoreProvider,
    LiveScoreboard, LiveScoreboardGame, LiveSlate, LiveTeamStats, ProviderOutcome,
    ProviderResponse, RawBody,
};
pub use nfl_model::live;
pub use nfl_model::{
    DfsPosition, Game, Player, PlayerWeekStats, Season, SeasonType, SeasonWeek, TeamAbbr,
    TeamWeekStats, Week, WeeklyRosterEntry, eastern_offset, to_eastern,
};
pub use nflverse_data::{
    Dataset, DatasetFreshness, DatasetReport, DatasetStatus, NflverseDataError, RefreshStatus,
    SyncReport,
};
pub use salaries::SalaryWeekOverview;
pub use store::PlayerLink;
pub use web::AdminConfig;
pub use web::router as admin_router;

pub struct WeeklySummary {
    pub season_week: SeasonWeek,
    pub salary_count: usize,
    pub unmatched_count: usize,
}

/// Identity fallback can read weekly rosters from any season.
fn update_is_relevant(update: &nflverse_data::AssetUpdate, season: Season) -> bool {
    update.season == Some(season)
        || matches!(
            update.dataset,
            Dataset::Schedules | Dataset::Players | Dataset::WeeklyRosters
        )
}

/// The stored slate state a build reads: metadata plus game membership.
async fn stored_slates(
    nfl: &NflData,
) -> Result<BTreeMap<SlateId, (Slate, Vec<String>)>, NflDataError> {
    let mut state = BTreeMap::new();
    for slate in slates::all(nfl).await? {
        let game_ids = store::slates::game_ids(nfl.store.reader(), &slate.id).await?;
        state.insert(slate.id, (slate, game_ids));
    }
    Ok(state)
}

pub struct NflData {
    /// The published snapshot; the shared handle lets a rebuild swap it while
    /// readers keep the snapshot they took.
    snapshot: ArcSwap<Snapshot>,
    provider: NflverseData,
    fanduel: fanduel_data::FanduelData,
    store: store::Store,
    refresh_interval: Option<std::time::Duration>,
    season: Season,
    /// Serializes slate reconciliation, builds, and publication.
    publication: tokio::sync::Mutex<()>,
    /// Receive end of the provider's sync events, taken once by
    /// [`Self::start_background_tasks`].
    sync_events: Mutex<Option<mpsc::UnboundedReceiver<SyncEvent>>>,
    /// The notification task; aborted on drop.
    update_worker: Mutex<Option<JoinHandle<()>>>,
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
    /// Start configured internal background work. Repeated calls are safe: the
    /// scheduler refreshes upstream datasets when configured, and the
    /// notification task republishes the snapshot after each relevant sync.
    pub fn start_background_tasks(self: &Arc<Self>) {
        if let Some(interval) = self.refresh_interval {
            self.provider.start_scheduler(interval);
        }
        self.start_update_worker();
    }

    /// Stop future automatic task triggers without cancelling an active
    /// refresh. The notification task keeps running: aborting it here would
    /// make a later start unable to restore its receiver.
    pub fn stop_background_tasks(&self) {
        self.provider.stop_scheduler();
    }

    /// Spawn the notification task; the receiver is taken on the first call, so
    /// later calls are no-ops.
    fn start_update_worker(self: &Arc<Self>) {
        let Some(mut sync_events) = self
            .sync_events
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
        else {
            return;
        };
        // A weak owner keeps a dropped handle droppable: the task upgrades
        // only to process one event and releases it before waiting again.
        let updates = Arc::downgrade(self);
        let worker = tokio::spawn(async move {
            while let Some(event) = sync_events.recv().await {
                let Some(nfl) = Weak::upgrade(&updates) else {
                    return;
                };
                nfl.apply_sync_event(event).await;
            }
        });
        *self
            .update_worker
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(worker);
    }

    /// Apply one completed sync: republish when the configured season's data
    /// changed, otherwise reconcile the slates only.
    async fn apply_sync_event(&self, event: SyncEvent) {
        let relevant = event
            .updates
            .iter()
            .any(|update| update_is_relevant(update, self.season));
        let outcome = if relevant {
            // A committed relevant asset rebuilds even when a sibling dataset
            // failed: its rows are in the store either way.
            self.reconcile_slates_and_rebuild().await
        } else if event.all_ok {
            // An unchanged or historical-only sync still reconciles the
            // slates, and republishes only when that changed what a build
            // reads.
            self.reconcile_slates_if_changed().await
        } else {
            // Nothing committed that this season can see.
            return;
        };
        if let Err(error) = outcome {
            tracing::warn!(%error, "snapshot update after sync failed");
        }
    }

    /// Reconcile the configured season's slates and republish. Returns after
    /// the new snapshot is published, or leaves the old one on error.
    pub(crate) async fn reconcile_slates_and_rebuild(&self) -> Result<(), NflDataError> {
        let _publication = self.publication.lock().await;
        slates::refresh(self).await?;
        self.build_and_publish().await
    }

    /// Reconcile the slates, republishing only when their stored state changed.
    async fn reconcile_slates_if_changed(&self) -> Result<(), NflDataError> {
        let _publication = self.publication.lock().await;
        let before = stored_slates(self).await?;
        slates::refresh(self).await?;
        if stored_slates(self).await? == before {
            return Ok(());
        }
        self.build_and_publish().await
    }

    /// Rebuild the snapshot and publish it. A failed build keeps the snapshot
    /// readers already hold; the next request to rebuild tries again.
    pub(crate) async fn rebuild_snapshot(&self) -> Result<(), NflDataError> {
        let _publication = self.publication.lock().await;
        self.build_and_publish().await
    }

    /// [`Self::rebuild_snapshot`] with the publication lock already held.
    async fn build_and_publish(&self) -> Result<(), NflDataError> {
        let snapshot = Snapshot::build(self).await?;
        self.snapshot.store(Arc::new(snapshot));
        Ok(())
    }

    /// Read an opaque hint for changes in the complete provider freshness snapshot.
    pub async fn data_revision(&self) -> Result<DataRevision, NflDataError> {
        let snapshot = self.provider.freshness().await?;
        Ok(DataRevision::from_snapshot(
            snapshot
                .into_iter()
                .map(|row| (row.dataset, row.assets, row.last_synced_at))
                .collect(),
        ))
    }

    pub fn parse_fanduel_csv(
        &self,
        season_week: SeasonWeek,
        csv_data: &[u8],
    ) -> Result<Vec<SalariedPlayer>, NflDataError> {
        self.fanduel
            .parse_fanduel_csv(season_week, csv_data)
            .map_err(NflDataError::Fanduel)
    }

    /// Persist typed FanDuel source rows. Callers republish the snapshot at
    /// their workflow boundary, once the whole import is persisted.
    pub async fn save_fanduel_salaries(
        &self,
        salaries: &[SalariedPlayer],
    ) -> Result<(), NflDataError> {
        self.fanduel
            .save_salaried_players(salaries)
            .await
            .map_err(NflDataError::Fanduel)
    }

    /// Persist FanDuel→GSIS player links. Callers republish the snapshot at
    /// their workflow boundary, once the whole import is persisted.
    pub(crate) async fn save_player_links(&self, links: &[PlayerLink]) -> Result<(), NflDataError> {
        if links.is_empty() {
            return Ok(());
        }
        store::player_link::save_links(&self.store, links).await?;
        Ok(())
    }

    /// Read stored FanDuel salary source rows for an exact contest week.
    pub async fn salaried_players_for(
        &self,
        season_week: SeasonWeek,
    ) -> Result<Vec<SalariedPlayer>, NflDataError> {
        self.fanduel
            .salaried_players_for(season_week)
            .await
            .map_err(NflDataError::Fanduel)
    }

    pub async fn fanduel_weekly_summaries(
        &self,
        matched_ids: &[u32],
    ) -> Result<Vec<WeeklySummary>, NflDataError> {
        let salary_counts = self.fanduel.weekly_salary_counts().await?;
        let unmatched_counts = self.fanduel.weekly_unmatched_counts(matched_ids).await?;

        let summaries = salary_counts
            .into_iter()
            .map(|(season_week, salary_count)| {
                let unmatched_count = unmatched_counts.get(&season_week).copied().unwrap_or(0);
                WeeklySummary {
                    season_week,
                    salary_count,
                    unmatched_count,
                }
            })
            .collect();

        Ok(summaries)
    }

    pub fn snapshot(&self) -> Arc<Snapshot> {
        self.snapshot.load_full()
    }

    /// Opens all databases, runs their migrations, reconciles the configured
    /// season's slates, and builds the initial snapshot. Any failure is a
    /// connection error; background work starts only through
    /// [`Self::start_background_tasks`].
    pub async fn connect(config: NflDataConfig) -> Result<Self, NflDataError> {
        let refresh_interval = config
            .refresh_interval
            .filter(|interval| !interval.is_zero());
        let (sync_events, sync_events_receiver) = mpsc::unbounded_channel();
        let provider =
            NflverseData::connect_with_sync_events(config.clone().into(), Some(sync_events))
                .await?;
        let fanduel = if config.fanduel_database_url == "sqlite::memory:" {
            fanduel_data::FanduelData::in_memory().await
        } else {
            fanduel_data::FanduelData::connect(&config.fanduel_database_url).await
        }?;
        let store = store::Store::open(&config.database_url).await?;

        let nfl_data = Self {
            provider,
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
        slates::refresh(&nfl_data).await?;
        nfl_data.build_and_publish().await?;

        Ok(nfl_data)
    }

    /// A fresh, empty, in-memory cache: migrations applied, nothing synced.
    /// Nothing builds a snapshot behind the caller's back: seeding tests
    /// republish explicitly.
    pub async fn in_memory() -> Result<Self, NflDataError> {
        let provider = NflverseData::in_memory().await?;
        let fanduel = fanduel_data::FanduelData::in_memory().await?;
        let store = store::Store::in_memory().await?;
        Ok(Self {
            provider,
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

    /// Reconcile this handle's season into the store now, returning every
    /// stored slate.
    ///
    /// Not needed for reads: slates populate at startup and after each
    /// successful sync, and lookups answer from storage. This is the explicit
    /// rebuild path.
    pub async fn refresh_slates(
        &self,
    ) -> Result<std::collections::BTreeSet<slates::Slate>, NflDataError> {
        slates::refresh(self).await
    }

    /// Display facts per gsis id: `players` first, weekly rosters for the
    /// misses; unknown ids are absent from the map.
    pub async fn identify(
        &self,
        ids: &[nfl_model::NflPlayerId],
    ) -> Result<HashMap<nfl_model::NflPlayerId, PlayerIdentity>, NflDataError> {
        identity::identify(&self.provider, ids).await
    }

    /// Seed weekly roster entries directly, for tests and tooling.
    #[doc(hidden)]
    pub async fn seed_weekly_roster_for_test(
        &self,
        entries: &[WeeklyRosterEntry],
    ) -> Result<(), NflDataError> {
        self.provider
            .seed_weekly_roster_for_test(entries)
            .await
            .map_err(NflDataError::Nflverse)
    }

    /// Seed players and games directly, for tests and tooling.
    #[doc(hidden)]
    pub async fn seed_for_test(
        &self,
        players: &[Player],
        games: &[Game],
    ) -> Result<(), NflDataError> {
        self.provider
            .seed_for_test(players, games)
            .await
            .map_err(NflDataError::Nflverse)
    }

    /// All cached games for a season, ordered by week.
    pub async fn games(&self, season: Season) -> Result<Vec<Game>, NflDataError> {
        self.provider
            .games(season)
            .await
            .map_err(NflDataError::Nflverse)
    }

    /// All cached players, ordered by name.
    pub async fn players(&self) -> Result<Vec<Player>, NflDataError> {
        self.provider
            .players()
            .await
            .map_err(NflDataError::Nflverse)
    }

    /// Players for a small set of ids, keyed by gsis id. Ids with no player are
    /// absent from the map rather than an error — callers render a fallback.
    ///
    pub async fn players_by_gsis<I: AsRef<str>>(
        &self,
        ids: &[I],
    ) -> Result<HashMap<String, Player>, NflDataError> {
        self.provider
            .players_by_gsis(ids)
            .await
            .map_err(NflDataError::Nflverse)
    }

    /// A team's cached roster for one week, ordered by player name.
    pub async fn weekly_roster(
        &self,
        season: Season,
        week: Week,
        team: &TeamAbbr,
    ) -> Result<Vec<WeeklyRosterEntry>, NflDataError> {
        self.provider
            .weekly_roster(season, week, team)
            .await
            .map_err(NflDataError::Nflverse)
    }

    /// All players' cached stat lines for one week, ordered by player GSIS id.
    pub async fn player_week_stats(
        &self,
        season: Season,
        week: Week,
    ) -> Result<Vec<PlayerWeekStats>, NflDataError> {
        self.provider
            .player_week_stats(season, week)
            .await
            .map_err(NflDataError::Nflverse)
    }

    /// All teams' pbp-derived DST lines for one week (sacks, takeaways, TDs,
    /// blocks, conversion returns, and points_allowed = the opponent's
    /// offensive points). Ordered by team.
    pub async fn team_week_stats(
        &self,
        season: Season,
        week: Week,
    ) -> Result<Vec<TeamWeekStats>, NflDataError> {
        self.provider
            .team_week_stats(season, week)
            .await
            .map_err(NflDataError::Nflverse)
    }
    /// Seed player and team week stats directly, for tests and tooling. Reuses the
    /// sync-path `replace` inserts (per-season delete-then-insert). Not part of the
    /// stable API.
    #[doc(hidden)]
    pub async fn seed_week_stats_for_test(
        &self,
        season: Season,
        players: &[PlayerWeekStats],
        teams: &[TeamWeekStats],
    ) -> Result<(), NflDataError> {
        self.provider
            .seed_week_stats_for_test(season, players, teams)
            .await
            .map_err(NflDataError::Nflverse)
    }
}

#[cfg(test)]
mod identify_tests {
    use crate::{NflData, Season, Week};
    use crate::{Player, TeamAbbr, WeeklyRosterEntry};

    fn player(gsis: &str, name: &str) -> Player {
        Player {
            espn_id: None,
            gsis_id: gsis.into(),
            full_name: name.into(),
            first_name: None,
            last_name: None,
            position: Some("RB".into()),
            latest_team: Some(TeamAbbr::KC),
            headshot_url: None,
        }
    }

    fn roster_entry(gsis: &str, name: &str) -> WeeklyRosterEntry {
        WeeklyRosterEntry {
            season: Season(2025),
            espn_id: None,
            week: Week(1),
            team: TeamAbbr::BUF,
            gsis_id: Some(gsis.into()),
            full_name: name.into(),
            last_name: None,
            position: Some("WR".into()),
        }
    }

    #[tokio::test]
    async fn names_from_players_first_then_the_weekly_roster() {
        let nfl = NflData::in_memory().await.unwrap();
        nfl.seed_for_test(&[player("00-P", "Player Table")], &[])
            .await
            .unwrap();
        nfl.seed_weekly_roster_for_test(&[roster_entry("00-R", "Roster Only")])
            .await
            .unwrap();

        let got = nfl
            .identify(&[
                nfl_model::NflPlayerId("00-P".into()),
                nfl_model::NflPlayerId("00-R".into()),
                nfl_model::NflPlayerId("00-X".into()),
            ])
            .await
            .unwrap();

        let from_players = &got[&nfl_model::NflPlayerId("00-P".into())];
        assert_eq!(from_players.name, "Player Table");
        assert_eq!(from_players.team, Some(TeamAbbr::KC));
        assert_eq!(from_players.position.as_deref(), Some("RB"));

        let from_roster = &got[&nfl_model::NflPlayerId("00-R".into())];
        assert_eq!(from_roster.name, "Roster Only");
        assert_eq!(from_roster.team, Some(TeamAbbr::BUF));
        assert_eq!(from_roster.position.as_deref(), Some("WR"));

        // An id in neither source has no identity to report.
        assert!(!got.contains_key(&nfl_model::NflPlayerId("00-X".into())));
    }

    #[tokio::test]
    async fn identify_carries_headshot_url_from_players_table() {
        let nfl = NflData::in_memory().await.unwrap();
        let player = Player {
            gsis_id: "00-HEAD".into(),
            espn_id: None,
            full_name: "Photo Guy".into(),
            first_name: None,
            last_name: None,
            position: Some("QB".into()),
            latest_team: Some(TeamAbbr::KC),
            headshot_url: Some("https://img.example/photo.png".into()),
        };
        nfl.seed_for_test(&[player], &[]).await.unwrap();

        let out = nfl
            .identify(&[nfl_model::NflPlayerId("00-HEAD".into())])
            .await
            .unwrap();
        let id = out
            .get(&nfl_model::NflPlayerId("00-HEAD".into()))
            .expect("identified");
        assert_eq!(
            id.headshot_url.as_deref(),
            Some("https://img.example/photo.png")
        );
    }
}

#[cfg(test)]
mod seed_week_stats_tests {
    use crate::{NflData, Season, Week};

    #[tokio::test]
    async fn seeds_player_and_team_week_stats() {
        let nfl = NflData::in_memory().await.unwrap();
        let mut p = crate::PlayerWeekStats {
            season: Season(2025),
            week: Week(1),
            season_type: crate::SeasonType::Reg,
            gsis_id: "00-RB".into(),
            team: crate::TeamAbbr::KC,
            opponent: None,
            completions: 0,
            attempts: 0,
            passing_yards: 0,
            passing_tds: 0,
            passing_interceptions: 0,
            rushing_attempts: 0,
            rushing_yards: 0,
            rushing_tds: 0,
            targets: 0,
            receptions: 0,
            receiving_yards: 0,
            receiving_tds: 0,
            fumbles_lost: 0,
            two_point_conversions: 0,
            special_teams_tds: 0,
            fumble_recovery_tds: 0,
        };
        p.rushing_yards = 42;

        let team = crate::TeamWeekStats {
            season: Season(2025),
            week: Week(1),
            season_type: crate::SeasonType::Reg,
            team: crate::TeamAbbr::KC,
            opponent: crate::TeamAbbr::BUF,
            game_id: "2025_01_BUF_KC".into(),
            sacks: 3,
            interceptions: 1,
            fumble_recoveries: 0,
            safeties: 0,
            touchdowns: 0,
            blocked_kicks: 0,
            conversion_returns: 0,
            points_allowed: 17,
        };
        p.rushing_tds = 1;
        nfl.seed_week_stats_for_test(Season(2025), &[p], std::slice::from_ref(&team))
            .await
            .unwrap();
        let got = nfl.player_week_stats(Season(2025), Week(1)).await.unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].gsis_id, "00-RB");
        assert_eq!(got[0].rushing_yards, 42);
        assert_eq!(
            nfl.team_week_stats(Season(2025), Week(1)).await.unwrap(),
            [team]
        );
        assert_eq!(got[0].rushing_tds, 1);
        assert!(
            nfl.provider
                .freshness()
                .await
                .unwrap()
                .iter()
                .all(|row| row.assets == 0)
        );
    }
}
