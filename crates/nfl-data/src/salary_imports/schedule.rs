use std::collections::{BTreeMap, BTreeSet, HashMap};

use nfl_model::{Game, SeasonType};

#[derive(Debug, PartialEq)]
pub(crate) enum ScheduleError {
    NoWeekContainsSlate,
}

pub(crate) fn resolve_slate_games(
    season_games: &[Game],
    matchups: &BTreeSet<(String, String)>,
) -> Result<Vec<Game>, ScheduleError> {
    if matchups.is_empty() {
        return Err(ScheduleError::NoWeekContainsSlate);
    }
    let mut by_context: BTreeMap<(u8, bool), HashMap<(&str, &str), &Game>> = BTreeMap::new();
    for game in season_games {
        by_context
            .entry((game.week.0, game.season_type == SeasonType::Post))
            .or_default()
            .insert((game.away_team.0.as_str(), game.home_team.0.as_str()), game);
    }
    let Some((_, context_games)) = by_context.iter().rev().find(|(_, games)| {
        matchups
            .iter()
            .all(|(away, home)| games.contains_key(&(away.as_str(), home.as_str())))
    }) else {
        return Err(ScheduleError::NoWeekContainsSlate);
    };
    Ok(matchups
        .iter()
        .map(|(away, home)| context_games[&(away.as_str(), home.as_str())].clone())
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use nfl_model::{Season, SeasonType, TeamAbbr, Week};
    use time::macros::datetime;

    fn game(id: &str, week: u8, home: &str, away: &str) -> Game {
        Game {
            gsis_game_id: id.into(),
            season: Season(2025),
            week: Week(week),
            season_type: SeasonType::Reg,
            kickoff: Some(datetime!(2025-09-07 17:00 UTC)),
            home_team: TeamAbbr(home.into()),
            away_team: TeamAbbr(away.into()),
            home_score: None,
            away_score: None,
        }
    }

    fn matchup_set(pairs: &[(&str, &str)]) -> BTreeSet<(String, String)> {
        pairs
            .iter()
            .map(|(a, h)| ((*a).into(), (*h).into()))
            .collect()
    }

    #[test]
    fn latest_common_week_includes_postseason() {
        let games = vec![
            game("week-1-repeat", 1, "SEA", "SF"),
            game("week-1-other", 1, "NYJ", "BUF"),
            game("week-20-repeat", 20, "SEA", "SF"),
            game("week-20-other", 20, "NYJ", "BUF"),
        ];
        let games = games
            .into_iter()
            .map(|mut game| {
                if game.week.0 == 20 {
                    game.season_type = SeasonType::Post;
                }
                game
            })
            .collect::<Vec<_>>();
        let resolved =
            resolve_slate_games(&games, &matchup_set(&[("SF", "SEA"), ("BUF", "NYJ")])).unwrap();
        assert_eq!(
            resolved
                .iter()
                .map(|g| g.gsis_game_id.as_str())
                .collect::<Vec<_>>(),
            ["week-20-other", "week-20-repeat"]
        );
    }

    #[test]
    fn full_slate_unique_to_week_one_still_chooses_week_one() {
        let games = vec![
            game("week-1-repeat", 1, "SEA", "SF"),
            game("week-1-only", 1, "NYJ", "BUF"),
            game("week-20-repeat", 20, "SEA", "SF"),
        ];
        let resolved =
            resolve_slate_games(&games, &matchup_set(&[("SF", "SEA"), ("BUF", "NYJ")])).unwrap();
        assert_eq!(
            resolved
                .iter()
                .map(|g| g.gsis_game_id.as_str())
                .collect::<Vec<_>>(),
            ["week-1-only", "week-1-repeat"]
        );
    }

    #[test]
    fn matchups_from_different_season_types_cannot_be_combined() {
        let mut postseason = game("postseason", 20, "NYJ", "BUF");
        postseason.season_type = SeasonType::Post;
        let games = vec![game("regular", 20, "SEA", "SF"), postseason];
        assert_eq!(
            resolve_slate_games(&games, &matchup_set(&[("SF", "SEA"), ("BUF", "NYJ")])),
            Err(ScheduleError::NoWeekContainsSlate)
        );
    }

    #[test]
    fn empty_matchup_set_is_unresolvable() {
        let games = vec![game("regular", 1, "SEA", "SF")];
        assert_eq!(
            resolve_slate_games(&games, &BTreeSet::new()),
            Err(ScheduleError::NoWeekContainsSlate)
        );
    }
}
