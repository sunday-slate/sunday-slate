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

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use arc_swap::ArcSwap;
use nflverse_data::NflverseData;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::snapshot::Snapshot;
use crate::snapshot::SnapshotSources;
use crate::snapshot::updates::UpdateInputs;
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

pub struct NflData {
    /// The published snapshot; the shared handle lets the update worker swap
    /// it while readers keep the snapshot they took.
    snapshot: Arc<ArcSwap<Snapshot>>,
    provider: NflverseData,
    fanduel: fanduel_data::FanduelData,
    store: store::Store,
    refresh_interval: Option<std::time::Duration>,
    season: Season,
    /// The update worker's input receivers, taken once by
    /// [`Self::start_background_tasks`].
    update_inputs: Mutex<Option<UpdateInputs>>,
    /// Asks the update worker to rebuild; saves send it after their
    /// persistence commits.
    snapshot_changes_tx: mpsc::UnboundedSender<()>,
    /// The update worker's task; aborted on drop.
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
    /// scheduler refreshes upstream datasets when configured, and the update
    /// worker keeps the slates and published snapshot current.
    pub fn start_background_tasks(&self) {
        if let Some(interval) = self.refresh_interval {
            self.provider.start_scheduler(interval);
        }
        self.start_update_worker();
    }

    /// Stop future automatic task triggers without cancelling an active
    /// refresh. The update worker keeps running: aborting it here would make a
    /// later start unable to restore its receivers.
    pub fn stop_background_tasks(&self) {
        self.provider.stop_scheduler();
    }

    /// Spawn the update worker; the receivers are taken on the first call, so
    /// later calls are no-ops.
    fn start_update_worker(&self) {
        let Some(inputs) = self
            .update_inputs
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
        else {
            return;
        };
        let worker = tokio::spawn(crate::snapshot::updates::run(
            SnapshotSources::from(self),
            Arc::clone(&self.snapshot),
            inputs,
        ));
        *self
            .update_worker
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(worker);
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

    /// Persist typed FanDuel source rows. A save touching the configured
    /// season notifies the update worker after the persistence commits.
    pub async fn save_fanduel_salaries(
        &self,
        salaries: &[SalariedPlayer],
    ) -> Result<(), NflDataError> {
        self.fanduel
            .save_salaried_players(salaries)
            .await
            .map_err(NflDataError::Fanduel)?;
        if salaries
            .iter()
            .any(|salary| salary.game.season_week.season == self.season())
        {
            self.notify_snapshot_change();
        }
        Ok(())
    }

    /// Persist FanDuel→GSIS player links, notifying the update worker on a
    /// successful nonempty save.
    pub(crate) async fn save_player_links(&self, links: &[PlayerLink]) -> Result<(), NflDataError> {
        if links.is_empty() {
            return Ok(());
        }
        store::player_link::save_links(&self.store, links).await?;
        self.notify_snapshot_change();
        Ok(())
    }

    /// Ask the update worker to rebuild. A closed receiver is ignored: the
    /// worker is optional.
    fn notify_snapshot_change(&self) {
        let _ = self.snapshot_changes_tx.send(());
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

    pub(crate) fn store_snapshot(&self, next: Snapshot) {
        self.snapshot.store(Arc::new(next));
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
        let (snapshot_changes_tx, snapshot_changes) = mpsc::unbounded_channel();
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
            snapshot: Arc::new(ArcSwap::from_pointee(Snapshot::default())),
            update_inputs: Mutex::new(Some(UpdateInputs {
                sync_events: sync_events_receiver,
                snapshot_changes,
            })),
            snapshot_changes_tx,
            update_worker: Mutex::new(None),
        };

        // The initial build reads stored slates, so reconcile them first.
        slates::refresh(&nfl_data).await?;

        let snapshot = Snapshot::build(&nfl_data).await?;
        nfl_data.store_snapshot(snapshot);

        Ok(nfl_data)
    }

    /// A fresh, empty, in-memory cache: migrations applied, nothing synced.
    /// The provider still emits sync events to this handle's update worker.
    pub async fn in_memory() -> Result<Self, NflDataError> {
        let (sync_events, sync_events_receiver) = mpsc::unbounded_channel();
        let provider = NflverseData::connect_with_sync_events(
            nflverse_data::NflverseDataConfig {
                database_url: "sqlite::memory:".into(),
                ..Default::default()
            },
            Some(sync_events),
        )
        .await?;
        let (snapshot_changes_tx, snapshot_changes) = mpsc::unbounded_channel();
        let fanduel = fanduel_data::FanduelData::in_memory().await?;
        let store = store::Store::in_memory().await?;
        Ok(Self {
            provider,
            fanduel,
            store,
            refresh_interval: None,
            season: Season::default(),
            snapshot: Arc::new(ArcSwap::from_pointee(Snapshot::default())),
            update_inputs: Mutex::new(Some(UpdateInputs {
                sync_events: sync_events_receiver,
                snapshot_changes,
            })),
            snapshot_changes_tx,
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
    pub async fn identify<I: AsRef<str>>(
        &self,
        ids: &[I],
    ) -> Result<HashMap<String, PlayerIdentity>, NflDataError> {
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

        let got = nfl.identify(&["00-P", "00-R", "00-X"]).await.unwrap();

        let from_players = &got["00-P"];
        assert_eq!(from_players.name, "Player Table");
        assert_eq!(from_players.team, Some(TeamAbbr::KC));
        assert_eq!(from_players.position.as_deref(), Some("RB"));

        let from_roster = &got["00-R"];
        assert_eq!(from_roster.name, "Roster Only");
        assert_eq!(from_roster.team, Some(TeamAbbr::BUF));
        assert_eq!(from_roster.position.as_deref(), Some("WR"));

        // An id in neither source has no identity to report.
        assert!(!got.contains_key("00-X"));
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

        let out = nfl.identify(&["00-HEAD"]).await.unwrap();
        let id = out.get("00-HEAD").expect("identified");
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
