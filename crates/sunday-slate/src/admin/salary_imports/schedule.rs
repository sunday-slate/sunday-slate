use std::collections::{BTreeMap, BTreeSet, HashMap};

use nfl_data::Game;

#[derive(Debug, PartialEq)]
pub enum ScheduleError {
    /// No single NFL week's games contain every slate matchup.
    NoWeekContainsSlate,
}

/// Resolve the games of a slate from its directed matchups.
///
/// Each FanDuel row carries a directed `AWAY@HOME` matchup. A directed matchup is
/// NOT unique across a full season: a same-venue playoff rematch repeats a
/// regular-season matchup (e.g. `SF@SEA` in both week 1 and a divisional game),
/// and `nfl.games` returns regular season and postseason together. A slate is
/// always one NFL week, so the whole slate pins the week: pick the earliest week
/// whose games contain every slate matchup, then map each matchup to that week's
/// game. `matchups` holds `(away, home)` in nflverse codes; the caller normalizes
/// FanDuel's native codes first.
pub fn resolve_slate_games(
    season_games: &[Game],
    matchups: &BTreeSet<(String, String)>,
) -> Result<Vec<Game>, ScheduleError> {
    let mut by_week: BTreeMap<u8, HashMap<(&str, &str), &Game>> = BTreeMap::new();
    for g in season_games {
        by_week
            .entry(g.week.0)
            .or_default()
            .insert((g.away_team.0.as_str(), g.home_team.0.as_str()), g);
    }

    // BTreeMap iterates weeks in ascending order, so `find` yields the earliest
    // week that fully contains the slate.
    let Some(week) = by_week.values().find(|week| {
        matchups
            .iter()
            .all(|(a, h)| week.contains_key(&(a.as_str(), h.as_str())))
    }) else {
        return Err(ScheduleError::NoWeekContainsSlate);
    };

    Ok(matchups
        .iter()
        .map(|(a, h)| week[&(a.as_str(), h.as_str())].clone())
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use nfl_data::{Season, SeasonType, TeamAbbr as NflTeamAbbr, Week};
    use time::OffsetDateTime;
    use time::macros::datetime;

    fn game(id: &str, week: u8, home: &str, away: &str, kickoff: OffsetDateTime) -> Game {
        Game {
            gsis_game_id: id.into(),
            season: Season(2025),
            week: Week(week),
            season_type: SeasonType::Reg,
            kickoff: Some(kickoff),
            home_team: NflTeamAbbr(home.into()),
            away_team: NflTeamAbbr(away.into()),
            home_score: None,
            away_score: None,
        }
    }

    fn matchups(pairs: &[(&str, &str)]) -> BTreeSet<(String, String)> {
        pairs
            .iter()
            .map(|(a, h)| (a.to_string(), h.to_string()))
            .collect()
    }

    #[test]
    fn resolves_each_matchup_to_its_game() {
        let games = vec![
            game(
                "2025_05_LA_SF",
                5,
                "SF",
                "LA",
                datetime!(2025-10-05 17:00 UTC),
            ),
            game(
                "2025_05_BUF_KC",
                5,
                "KC",
                "BUF",
                datetime!(2025-10-05 20:00 UTC),
            ),
        ];
        let got = resolve_slate_games(&games, &matchups(&[("LA", "SF"), ("BUF", "KC")]))
            .expect("resolve");
        let ids: BTreeSet<_> = got.iter().map(|g| g.gsis_game_id.clone()).collect();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains("2025_05_LA_SF"));
        assert!(ids.contains("2025_05_BUF_KC"));
    }

    #[test]
    fn partial_slate_resolves_only_its_matchups() {
        let games = vec![
            game(
                "2025_05_LA_SF",
                5,
                "SF",
                "LA",
                datetime!(2025-10-05 17:00 UTC),
            ),
            game(
                "2025_05_BUF_KC",
                5,
                "KC",
                "BUF",
                datetime!(2025-10-05 20:00 UTC),
            ),
        ];
        let got = resolve_slate_games(&games, &matchups(&[("LA", "SF")])).expect("resolve");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].gsis_game_id, "2025_05_LA_SF");
    }

    #[test]
    fn directed_matchup_disambiguates_a_division_rematch() {
        let games = vec![
            game(
                "2025_05_LA_SF",
                5,
                "SF",
                "LA",
                datetime!(2025-10-05 17:00 UTC),
            ),
            game(
                "2025_15_SF_LA",
                15,
                "LA",
                "SF",
                datetime!(2025-12-14 17:00 UTC),
            ),
        ];
        let got = resolve_slate_games(&games, &matchups(&[("SF", "LA")])).expect("resolve");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].gsis_game_id, "2025_15_SF_LA");
    }

    #[test]
    fn full_slate_pins_the_week_when_a_matchup_repeats_in_the_playoffs() {
        // SF@SEA occurs in both the regular season (week 1) and the divisional
        // round (week 20). A lone SF@SEA is ambiguous, but the rest of the week-1
        // slate pins the week, so the regular-season game is chosen.
        let games = vec![
            game(
                "2025_01_SF_SEA",
                1,
                "SEA",
                "SF",
                datetime!(2025-09-07 20:00 UTC),
            ),
            game(
                "2025_01_BUF_NYJ",
                1,
                "NYJ",
                "BUF",
                datetime!(2025-09-07 17:00 UTC),
            ),
            game(
                "2025_20_SF_SEA",
                20,
                "SEA",
                "SF",
                datetime!(2026-01-18 20:00 UTC),
            ),
        ];
        let got = resolve_slate_games(&games, &matchups(&[("SF", "SEA"), ("BUF", "NYJ")]))
            .expect("resolve");
        let ids: BTreeSet<_> = got.iter().map(|g| g.gsis_game_id.clone()).collect();
        assert!(ids.contains("2025_01_SF_SEA"));
        assert!(ids.contains("2025_01_BUF_NYJ"));
        assert!(!ids.contains("2025_20_SF_SEA"));
    }

    #[test]
    fn errors_when_no_week_contains_the_whole_slate() {
        let games = vec![game(
            "2025_05_LA_SF",
            5,
            "SF",
            "LA",
            datetime!(2025-10-05 17:00 UTC),
        )];
        assert_eq!(
            resolve_slate_games(&games, &matchups(&[("KC", "BUF")])),
            Err(ScheduleError::NoWeekContainsSlate)
        );
    }
}
