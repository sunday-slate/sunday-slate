mod config;
mod data_revision;
mod identity;
mod salaries;
#[cfg(test)]
mod test;
mod web;

use std::collections::HashMap;

use nflverse_data::NflverseData;

pub use config::NflDataConfig;
pub use data_revision::DataRevision;
pub use identity::PlayerIdentity;
pub use live::{
    EspnPlayerId, InjuryDesignation, InjuryEntry, InjuryProvider, InjuryReport, LiveGame,
    LiveGamePhase, LiveGameSnapshot, LivePlayerStats, LiveProviderError, LiveScoreProvider,
    LiveScoreboard, LiveScoreboardGame, LiveSlate, LiveTeamStats, ProviderOutcome,
    ProviderResponse, RawBody,
};
pub use nfl_model::live;
pub use nfl_model::{
    DfsPosition, Game, Player, PlayerWeekStats, Season, SeasonType, TeamAbbr, TeamWeekStats, Week,
    WeeklyRosterEntry, eastern_offset, to_eastern,
};
pub use nflverse_data::{
    Dataset, DatasetFreshness, DatasetReport, DatasetStatus, NflverseDataError as NflDataError,
    RefreshStatus, SyncReport,
};
pub use salaries::{
    SalaryCsvDiagnostic, SalaryMatchup, SalaryRow, SalaryRowDiagnostic, SalaryUploadError,
    SalaryUploadOutcome,
};
pub use web::router as admin_router;

#[derive(Debug, thiserror::Error)]
pub enum NflDataConnectError {
    #[error("nflverse provider startup failed: {0}")]
    Nflverse(#[source] NflDataError),
    #[error("FanDuel provider startup failed: {0}")]
    Fanduel(#[source] fanduel_data::FanduelDataError),
}

pub struct NflData {
    provider: NflverseData,
    fanduel: fanduel_data::FanduelData,
    refresh_interval: Option<std::time::Duration>,
}

impl NflData {
    /// Start configured internal background work. Repeated calls are safe.
    pub fn start_background_tasks(&self) {
        if let Some(interval) = self.refresh_interval {
            self.provider.start_scheduler(interval);
        }
    }

    /// Stop future automatic task triggers without cancelling an active refresh.
    pub fn stop_background_tasks(&self) {
        self.provider.stop_scheduler();
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

    /// Archives and interprets a FanDuel salary upload without exposing archive identities.
    pub async fn receive_salary_upload(
        &self,
        bytes: &[u8],
        filename: Option<&str>,
    ) -> Result<SalaryUploadOutcome, SalaryUploadError> {
        let received = self.fanduel.receive(bytes, filename).await?;
        Ok(received.interpretation.into())
    }

    /// Opens both provider databases and runs their migrations.
    pub async fn connect(config: NflDataConfig) -> Result<Self, NflDataConnectError> {
        let refresh_interval = config
            .refresh_interval
            .filter(|interval| !interval.is_zero());
        let provider = NflverseData::connect(config.clone().into())
            .await
            .map_err(NflDataConnectError::Nflverse)?;
        let fanduel = fanduel_data::FanduelData::connect(fanduel_data::FanduelDataConfig {
            database_url: config.fanduel_database_url,
        })
        .await
        .map_err(NflDataConnectError::Fanduel)?;
        Ok(Self {
            provider,
            fanduel,
            refresh_interval,
        })
    }

    /// A fresh, empty, in-memory cache — for tests and tooling that need an
    /// `NflData` handle without a database file. Migrations are applied; no data
    /// is synced.
    pub async fn in_memory() -> Result<Self, NflDataConnectError> {
        let provider = NflverseData::in_memory()
            .await
            .map_err(NflDataConnectError::Nflverse)?;
        let fanduel = fanduel_data::FanduelData::in_memory()
            .await
            .map_err(NflDataConnectError::Fanduel)?;
        Ok(Self {
            provider,
            fanduel,
            refresh_interval: None,
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
    ) -> Result<HashMap<String, WeeklyRosterEntry>, NflDataError> {
        self.provider.weekly_roster_entries_by_gsis(ids).await
    }

    /// Display facts for a set of gsis ids: the `players` table first, the
    /// weekly rosters for the ids it misses. An id in neither source is
    /// absent from the map.
    pub async fn identify<I: AsRef<str>>(
        &self,
        ids: &[I],
    ) -> Result<HashMap<String, PlayerIdentity>, NflDataError> {
        let players = self.players_by_gsis(ids).await?;
        let unnamed: Vec<&str> = ids
            .iter()
            .map(|id| id.as_ref())
            .filter(|id| !players.contains_key(*id))
            .collect();
        let roster = self.weekly_roster_entries_by_gsis(&unnamed).await?;
        let named = players.into_iter().map(|(id, p)| {
            let identity = PlayerIdentity {
                name: p.full_name,
                team: p.latest_team,
                position: p.position,
                headshot_url: p.headshot_url,
            };
            (id, identity)
        });
        let rostered = roster.into_iter().map(|(id, e)| {
            let identity = PlayerIdentity {
                name: e.full_name,
                team: Some(e.team),
                position: e.position,
                headshot_url: None,
            };
            (id, identity)
        });
        Ok(named.chain(rostered).collect())
    }

    /// Seed weekly roster entries directly, for tests and tooling. Not part of
    /// the stable API — bypasses the sync pipeline and `sync_state` bookkeeping.
    #[doc(hidden)]
    pub async fn seed_weekly_roster_for_test(
        &self,
        entries: &[WeeklyRosterEntry],
    ) -> Result<(), NflDataError> {
        self.provider.seed_weekly_roster_for_test(entries).await
    }

    /// Seed players and games directly, for tests and tooling. Not part of the
    /// stable API — bypasses the sync pipeline and `sync_state` bookkeeping.
    #[doc(hidden)]
    pub async fn seed_for_test(
        &self,
        players: &[Player],
        games: &[Game],
    ) -> Result<(), NflDataError> {
        self.provider.seed_for_test(players, games).await
    }

    /// All cached games for a season, ordered by week.
    pub async fn games(&self, season: Season) -> Result<Vec<Game>, NflDataError> {
        self.provider.games(season).await
    }

    /// All cached players, ordered by name.
    pub async fn players(&self) -> Result<Vec<Player>, NflDataError> {
        self.provider.players().await
    }

    /// Players for a small set of ids, keyed by gsis id. Ids with no player are
    /// absent from the map rather than an error — callers render a fallback.
    ///
    pub async fn players_by_gsis<I: AsRef<str>>(
        &self,
        ids: &[I],
    ) -> Result<HashMap<String, Player>, NflDataError> {
        self.provider.players_by_gsis(ids).await
    }

    /// A team's cached roster for one week, ordered by player name.
    pub async fn weekly_roster(
        &self,
        season: Season,
        week: Week,
        team: &TeamAbbr,
    ) -> Result<Vec<WeeklyRosterEntry>, NflDataError> {
        self.provider.weekly_roster(season, week, team).await
    }

    /// All players' cached stat lines for one week, ordered by player GSIS id.
    pub async fn player_week_stats(
        &self,
        season: Season,
        week: Week,
    ) -> Result<Vec<PlayerWeekStats>, NflDataError> {
        self.provider.player_week_stats(season, week).await
    }

    /// All teams' pbp-derived D/ST lines for one week (sacks, takeaways, TDs,
    /// blocks, conversion returns, and points_allowed = the opponent's
    /// offensive points). Ordered by team.
    pub async fn team_week_stats(
        &self,
        season: Season,
        week: Week,
    ) -> Result<Vec<TeamWeekStats>, NflDataError> {
        self.provider.team_week_stats(season, week).await
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
            latest_team: Some(TeamAbbr("KC".into())),
            headshot_url: None,
        }
    }

    fn roster_entry(gsis: &str, name: &str) -> WeeklyRosterEntry {
        WeeklyRosterEntry {
            season: Season(2025),
            espn_id: None,
            week: Week(1),
            team: TeamAbbr("BUF".into()),
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
        assert_eq!(from_players.team, Some(TeamAbbr("KC".into())));
        assert_eq!(from_players.position.as_deref(), Some("RB"));

        let from_roster = &got["00-R"];
        assert_eq!(from_roster.name, "Roster Only");
        assert_eq!(from_roster.team, Some(TeamAbbr("BUF".into())));
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
            latest_team: Some(TeamAbbr("KC".into())),
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
            team: crate::TeamAbbr("KC".into()),
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
            team: crate::TeamAbbr("KC".into()),
            opponent: crate::TeamAbbr("BUF".into()),
            gsis_game_id: "2025_01_BUF_KC".into(),
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
