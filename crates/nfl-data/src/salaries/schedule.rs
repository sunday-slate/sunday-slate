use std::collections::{BTreeSet, HashMap};

use crate::{Game, SeasonWeek};

#[derive(Debug, PartialEq)]
pub enum ScheduleError {
    /// The requested season week does not contain every slate matchup.
    NoWeekContainsSlate,
}

/// Resolve a slate's matchups to games. A directed matchup can recur across
/// the season (playoff rematch), so lookup is restricted to the requested
/// season, season type, and week.
pub fn resolve_slate_games(
    season_games: &[Game],
    season_week: &SeasonWeek,
    matchups: &BTreeSet<(String, String)>,
) -> Result<Vec<Game>, ScheduleError> {
    let week: HashMap<(&str, &str), &Game> = season_games
        .iter()
        .filter(|game| {
            game.season == season_week.season
                && game.season_type == season_week.season_type
                && game.week == season_week.week
        })
        .map(|game| ((game.away_team.as_str(), game.home_team.as_str()), game))
        .collect();

    if !matchups
        .iter()
        .all(|(away, home)| week.contains_key(&(away.as_str(), home.as_str())))
    {
        return Err(ScheduleError::NoWeekContainsSlate);
    }

    Ok(matchups
        .iter()
        .map(|(away, home)| week[&(away.as_str(), home.as_str())].clone())
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Season, SeasonType, SeasonWeek, Week};
    use time::OffsetDateTime;
    use time::macros::datetime;

    fn game(id: &str, week: u8, home: &str, away: &str, kickoff: OffsetDateTime) -> Game {
        Game {
            gsis_game_id: id.into(),
            season: Season(2025),
            week: Week(week),
            season_type: SeasonType::Reg,
            kickoff: Some(kickoff),
            home_team: home.parse().unwrap(),
            away_team: away.parse().unwrap(),
            home_score: None,
            away_score: None,
        }
    }

    fn game_type(mut game: Game, season_type: SeasonType) -> Game {
        game.season_type = season_type;
        game
    }

    fn season_week(season_type: SeasonType, week: u8) -> SeasonWeek {
        SeasonWeek {
            season: Season(2025),
            season_type,
            week: Week(week),
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
        let got = resolve_slate_games(
            &games,
            &season_week(SeasonType::Reg, 5),
            &matchups(&[("LA", "SF"), ("BUF", "KC")]),
        )
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
        let got = resolve_slate_games(
            &games,
            &season_week(SeasonType::Reg, 5),
            &matchups(&[("LA", "SF")]),
        )
        .expect("resolve");
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
        let got = resolve_slate_games(
            &games,
            &season_week(SeasonType::Reg, 15),
            &matchups(&[("SF", "LA")]),
        )
        .expect("resolve");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].gsis_game_id, "2025_15_SF_LA");
    }

    #[test]
    fn full_slate_pins_the_week_when_a_matchup_repeats_in_the_playoffs() {
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
        let got = resolve_slate_games(
            &games,
            &season_week(SeasonType::Reg, 1),
            &matchups(&[("SF", "SEA"), ("BUF", "NYJ")]),
        )
        .expect("resolve");
        let ids: BTreeSet<_> = got.iter().map(|g| g.gsis_game_id.clone()).collect();
        assert!(ids.contains("2025_01_SF_SEA"));
        assert!(ids.contains("2025_01_BUF_NYJ"));
        assert!(!ids.contains("2025_20_SF_SEA"));
    }

    #[test]
    fn exact_season_week_disambiguates_regular_and_postseason_rematches() {
        let regular = game(
            "2025_01_SF_SEA",
            1,
            "SEA",
            "SF",
            datetime!(2025-09-07 20:00 UTC),
        );
        let postseason = game_type(
            game(
                "2025_01_SF_SEA_POST",
                1,
                "SEA",
                "SF",
                datetime!(2026-01-18 20:00 UTC),
            ),
            SeasonType::Post,
        );
        let games = vec![regular, postseason];
        for (requested, expected) in [
            (season_week(SeasonType::Reg, 1), "2025_01_SF_SEA"),
            (season_week(SeasonType::Post, 1), "2025_01_SF_SEA_POST"),
        ] {
            let resolved = resolve_slate_games(&games, &requested, &matchups(&[("SF", "SEA")]))
                .expect("exact season week resolves");
            assert_eq!(resolved.len(), 1);
            assert_eq!(resolved[0].gsis_game_id, expected);
        }
    }

    #[test]
    fn missing_target_week_does_not_resolve_a_matchup_from_another_week() {
        let games = vec![game(
            "2025_01_SF_SEA",
            1,
            "SEA",
            "SF",
            datetime!(2025-09-07 20:00 UTC),
        )];
        assert_eq!(
            resolve_slate_games(
                &games,
                &season_week(SeasonType::Reg, 2),
                &matchups(&[("SF", "SEA")])
            ),
            Err(ScheduleError::NoWeekContainsSlate)
        );
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
            resolve_slate_games(
                &games,
                &season_week(SeasonType::Reg, 5),
                &matchups(&[("KC", "BUF")])
            ),
            Err(ScheduleError::NoWeekContainsSlate)
        );
    }
}
