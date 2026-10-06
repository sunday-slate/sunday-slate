mod client;
mod config;
use nfl_model::eastern;
mod error;
mod ingest;
pub use nfl_model::live;
mod model;
mod store;
mod sync;

use std::collections::HashMap;

use store::Store;

pub use config::NflDataConfig as NflverseDataConfig;
use config::NflDataConfig;
pub use eastern::{eastern_offset, to_eastern};
pub use error::NflDataError as NflverseDataError;
pub use live::{
    EspnPlayerId, InjuryDesignation, InjuryEntry, InjuryProvider, InjuryReport, LiveGame,
    LiveGamePhase, LiveGameSnapshot, LivePlayerStats, LiveProviderError, LiveScoreProvider,
    LiveScoreboard, LiveScoreboardGame, LiveSlate, LiveTeamStats, ProviderOutcome,
    ProviderResponse, RawBody,
};
pub use model::{
    Dataset, DatasetFreshness, DatasetReport, DatasetStatus, Game, Player, PlayerSeasonTotals,
    PlayerWeekStats, RosterEntry, Season, SeasonType, SyncReport, TeamAbbr, TeamWeekStats, Week,
    WeeklyRosterEntry,
};

pub struct NflverseData {
    store: Store,
    config: NflDataConfig,
}

impl NflverseData {
    /// Opens (creating if missing) the cache database and runs migrations.
    pub async fn connect(config: NflverseDataConfig) -> Result<Self, NflverseDataError> {
        let store = Store::open(&config.database_url).await?;
        Ok(Self { store, config })
    }

    /// A fresh, empty, in-memory cache — for tests and tooling that need an
    /// `NflData` handle without a database file. Migrations are applied; no data
    /// is synced.
    pub async fn in_memory() -> Result<Self, NflverseDataError> {
        Ok(Self {
            store: Store::in_memory().await?,
            config: NflDataConfig::default(),
        })
    }

    /// Most recent weekly roster entry for each id, keyed by gsis id. Ids with
    /// no entry are absent from the map rather than an error.
    ///
    /// Complements [`Self::players_by_gsis`]: candidate lists are built from
    /// weekly rosters, and 144 of 2025's 3,133 rostered players have no row in
    /// the `players` table, so a pick cannot always be named from there.
    pub async fn weekly_roster_entries_by_gsis<I: AsRef<str>>(
        &self,
        ids: &[I],
    ) -> Result<HashMap<String, WeeklyRosterEntry>, NflverseDataError> {
        let mut out = HashMap::with_capacity(ids.len());
        for id in ids {
            let gsis = id.as_ref();
            if out.contains_key(gsis) {
                continue;
            }
            if let Some(e) = store::weekly_rosters::by_gsis(self.store.reader(), gsis).await? {
                out.insert(gsis.to_string(), e);
            }
        }
        Ok(out)
    }

    /// Seed weekly roster entries directly, for tests and tooling. Not part of
    /// the stable API — bypasses the sync pipeline and `sync_state` bookkeeping.
    #[doc(hidden)]
    pub async fn seed_weekly_roster_for_test(
        &self,
        entries: &[WeeklyRosterEntry],
    ) -> Result<(), NflverseDataError> {
        self.store
            .write_tx(async |conn| store::weekly_rosters::seed(conn, entries).await)
            .await
    }

    /// Seed players and games directly, for tests and tooling. Not part of the
    /// stable API — bypasses the sync pipeline and `sync_state` bookkeeping.
    #[doc(hidden)]
    pub async fn seed_for_test(
        &self,
        players: &[Player],
        games: &[Game],
    ) -> Result<(), NflverseDataError> {
        self.store
            .write_tx(async |conn| {
                store::players::seed(conn, players).await?;
                store::games::seed(conn, games).await?;
                Ok(())
            })
            .await
    }

    /// All cached games for a season, ordered by week.
    pub async fn games(&self, season: Season) -> Result<Vec<Game>, NflverseDataError> {
        store::games::for_season(self.store.reader(), season).await
    }

    /// All cached players, ordered by name.
    pub async fn players(&self) -> Result<Vec<Player>, NflverseDataError> {
        store::players::all(self.store.reader()).await
    }

    /// Players for a small set of ids, keyed by gsis id. Ids with no player are
    /// absent from the map rather than an error — callers render a fallback.
    ///
    /// One indexed seek per id — ~25µs for a nine-slot lineup, so the loop is
    /// not worth batching at the sizes call sites actually pass. If that ever
    /// changes, the set-based form is `WHERE gsis_id IN (SELECT value FROM
    /// json_each(?))`: one bind parameter and static SQL, so `query_as!` still
    /// applies and no `FromRow` impl is needed. It does require `gsis_id` to
    /// lead an index — SQLite will not skip-scan an `IN` list, and drops to a
    /// full table scan without one.
    pub async fn players_by_gsis<I: AsRef<str>>(
        &self,
        ids: &[I],
    ) -> Result<HashMap<String, Player>, NflverseDataError> {
        let mut out = HashMap::with_capacity(ids.len());
        for id in ids {
            let gsis = id.as_ref();
            if out.contains_key(gsis) {
                continue;
            }
            if let Some(p) = store::players::by_gsis(self.store.reader(), gsis).await? {
                out.insert(gsis.to_string(), p);
            }
        }
        Ok(out)
    }

    /// A team's cached roster for a season, ordered by player name.
    pub async fn roster(
        &self,
        season: Season,
        team: &TeamAbbr,
    ) -> Result<Vec<RosterEntry>, NflverseDataError> {
        store::rosters::for_team(self.store.reader(), season, team).await
    }

    /// A team's cached roster for one week, ordered by player name. Prefer this
    /// over [`Self::roster`] when the question is "who was on this team *then*" —
    /// the season roster reflects only where each player ended up.
    pub async fn weekly_roster(
        &self,
        season: Season,
        week: Week,
        team: &TeamAbbr,
    ) -> Result<Vec<WeeklyRosterEntry>, NflverseDataError> {
        store::weekly_rosters::for_team_week(self.store.reader(), season, week, team).await
    }

    /// All players' cached stat lines for one week, ordered by player GSIS id.
    pub async fn player_week_stats(
        &self,
        season: Season,
        week: Week,
    ) -> Result<Vec<PlayerWeekStats>, NflverseDataError> {
        store::player_stats::for_week(self.store.reader(), season, week).await
    }

    /// Each player's summed PPR points and games played for the season's
    /// weeks before `before`. Players with no stat rows are absent.
    pub async fn player_season_totals(
        &self,
        season: Season,
        before: Week,
    ) -> Result<Vec<PlayerSeasonTotals>, NflverseDataError> {
        store::player_stats::totals_before(self.store.reader(), season, before).await
    }

    /// All teams' pbp-derived D/ST lines for one week (sacks, takeaways, TDs,
    /// blocks, conversion returns, and points_allowed = the opponent's
    /// offensive points). Ordered by team.
    pub async fn team_week_stats(
        &self,
        season: Season,
        week: Week,
    ) -> Result<Vec<TeamWeekStats>, NflverseDataError> {
        store::team_stats::for_week(self.store.reader(), season, week).await
    }
    /// Freshness for every dataset, in sync order.
    pub async fn freshness(&self) -> Result<Vec<DatasetFreshness>, NflverseDataError> {
        let rows = store::sync_state::freshness(self.store.reader()).await?;
        Ok(Dataset::ALL
            .into_iter()
            .map(|dataset| {
                let row = rows
                    .iter()
                    .find(|row| row.release_tag == dataset.release_tag());
                DatasetFreshness {
                    dataset,
                    assets: row.map_or(0, |row| row.assets),
                    last_synced_at: row.map(|row| row.last_synced_at),
                }
            })
            .collect())
    }

    /// Fetch all datasets from nflverse, skipping assets whose upstream
    /// updated_at matches the cache. Failures are isolated per dataset and
    /// reported in the SyncReport; Err is reserved for setup-level failures.
    /// For per-season datasets, isolation is finer still: a bad season's
    /// asset doesn't block the other seasons in the same dataset from
    /// updating.
    pub async fn sync(&self) -> Result<SyncReport, NflverseDataError> {
        sync::run(&self.store, &self.config).await
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
    ) -> Result<(), NflverseDataError> {
        let asset = store::sync_state::SyncedAsset {
            release_tag: "test",
            asset_name: "test",
            updated_at: "1970-01-01T00:00:00Z",
        };
        store::player_stats::replace(&self.store, &asset, season, players).await?;
        store::team_stats::replace(&self.store, &asset, season, teams).await?;
        Ok(())
    }
}

#[cfg(test)]
mod freshness_tests {
    use crate::{Dataset, NflverseData};

    #[tokio::test]
    async fn returns_fixed_order_and_ignores_unknown_tags() {
        let nfl = NflverseData::in_memory().await.unwrap();
        for (tag, asset) in [
            ("test", "ignored"),
            ("schedules", "games.csv"),
            ("schedules", "extra.csv"),
        ] {
            sqlx::query(
                "INSERT INTO sync_state (release_tag, asset_name, upstream_updated_at) \
                 VALUES (?, ?, ?)",
            )
            .bind(tag)
            .bind(asset)
            .bind("2026-01-01T00:00:00Z")
            .execute(nfl.store.reader())
            .await
            .unwrap();
        }

        let rows = nfl.freshness().await.unwrap();
        assert_eq!(
            rows.iter().map(|row| row.dataset).collect::<Vec<_>>(),
            Dataset::ALL
        );
        assert_eq!(rows[0].assets, 2);
        assert!(rows[0].last_synced_at.is_some());
        assert_eq!(rows[1].assets, 0);
        assert!(rows[1].last_synced_at.is_none());
    }
}
