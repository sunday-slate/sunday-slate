//! nflverse-backed reads and test seeding.

use nflverse_data::NflverseData;

use crate::{
    Game, NflData, NflDataError, Player, PlayerWeekStats, Season, SeasonWeek, TeamAbbr,
    TeamWeekStats, WeeklyRosterEntry,
};

/// All cached games for a season, ordered by week.
pub(crate) async fn games(
    nflverse: &NflverseData,
    season: Season,
) -> Result<Vec<Game>, NflDataError> {
    nflverse.games(season).await.map_err(NflDataError::Nflverse)
}

impl NflData {
    /// All cached players, ordered by name.
    pub async fn players(&self) -> Result<Vec<Player>, NflDataError> {
        self.nflverse
            .players()
            .await
            .map_err(NflDataError::Nflverse)
    }

    /// A team's cached roster for one week, ordered by player name.
    pub async fn weekly_roster(
        &self,
        season_week: SeasonWeek,
        team: &TeamAbbr,
    ) -> Result<Vec<WeeklyRosterEntry>, NflDataError> {
        self.nflverse
            .weekly_roster(season_week, team)
            .await
            .map_err(NflDataError::Nflverse)
    }

    /// All players' cached stat lines for one week, ordered by player GSIS id.
    pub async fn player_week_stats(
        &self,
        season_week: SeasonWeek,
    ) -> Result<Vec<PlayerWeekStats>, NflDataError> {
        self.nflverse
            .player_week_stats(season_week)
            .await
            .map_err(NflDataError::Nflverse)
    }

    /// All teams' pbp-derived DST lines for one week (sacks, takeaways, TDs,
    /// blocks, conversion returns, and points_allowed = the opponent's
    /// offensive points). Ordered by team.
    pub async fn team_week_stats(
        &self,
        season_week: SeasonWeek,
    ) -> Result<Vec<TeamWeekStats>, NflDataError> {
        self.nflverse
            .team_week_stats(season_week)
            .await
            .map_err(NflDataError::Nflverse)
    }

    /// Seed weekly roster entries directly, for tests and tooling.
    #[doc(hidden)]
    pub async fn seed_weekly_roster_for_test(
        &self,
        entries: &[WeeklyRosterEntry],
    ) -> Result<(), NflDataError> {
        self.nflverse
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
        self.nflverse
            .seed_for_test(players, games)
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
        self.nflverse
            .seed_week_stats_for_test(season, players, teams)
            .await
            .map_err(NflDataError::Nflverse)
    }
}

#[cfg(test)]
mod tests {
    use crate::{NflData, Season, SeasonWeek, Week};

    #[tokio::test]
    async fn seeds_player_and_team_week_stats() {
        let nfl = NflData::in_memory().await.unwrap();
        let mut p = crate::PlayerWeekStats {
            season_week: SeasonWeek::new(Season(2025), Week(1)).unwrap(),
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
            season_week: SeasonWeek::new(Season(2025), Week(1)).unwrap(),
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
        let week = SeasonWeek::new(Season(2025), Week(1)).unwrap();
        let got = nfl.player_week_stats(week).await.unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].gsis_id, "00-RB");
        assert_eq!(got[0].rushing_yards, 42);
        assert_eq!(nfl.team_week_stats(week).await.unwrap(), [team]);
        assert_eq!(got[0].rushing_tds, 1);
        assert!(
            nfl.nflverse
                .freshness()
                .await
                .unwrap()
                .iter()
                .all(|row| row.assets == 0)
        );
    }
}
