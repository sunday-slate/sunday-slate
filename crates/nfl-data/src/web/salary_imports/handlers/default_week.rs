use crate::{Game, Season, SeasonType, SeasonWeek, Week, to_eastern};
use nfl_model::nfl_week_monday;

/// Select the next contest week, treating Monday night as belonging to the week just played.
pub(crate) fn default_season_week(
    games: &[Game],
    configured_season: Season,
    now: time::OffsetDateTime,
) -> SeasonWeek {
    let now = to_eastern(now);
    let week_start =
        now.date() - time::Duration::days(i64::from(now.weekday().number_days_from_monday()));
    let mut weeks = games
        .iter()
        .filter(|g| {
            g.season_week.season == configured_season
                && g.kickoff.is_some_and(|kickoff| {
                    kickoff > now && nfl_week_monday(to_eastern(kickoff).date()) >= week_start
                })
        })
        .collect::<Vec<_>>();
    weeks.sort_by_key(|game| game.kickoff);
    weeks.first().map(|g| g.season_week).unwrap_or(SeasonWeek {
        season: configured_season,
        season_type: SeasonType::Reg,
        week: Week(1),
    })
}

#[cfg(test)]
mod default_week_tests {
    use super::*;
    use time::macros::datetime;

    fn game(id: &str, week: u8, kickoff: time::OffsetDateTime) -> Game {
        Game {
            game_id: id.into(),
            season_week: SeasonWeek {
                season: Season(2025),
                season_type: SeasonType::Reg,
                week: Week(week),
            },
            kickoff: Some(kickoff),
            home_team: "KC".parse().unwrap(),
            away_team: "BUF".parse().unwrap(),
            home_score: None,
            away_score: None,
        }
    }

    #[test]
    fn monday_boundary_chooses_the_next_contest_week() {
        let games = vec![
            game("played-mnf", 5, datetime!(2025-10-06 23:15 UTC)),
            game("next", 6, datetime!(2025-10-12 17:00 UTC)),
        ];
        assert_eq!(
            default_season_week(&games, Season(2025), datetime!(2025-10-06 18:00 UTC)).week,
            Week(6)
        );
    }

    #[test]
    fn completed_games_in_the_current_week_do_not_prevent_fallback() {
        let games = vec![game("past", 5, datetime!(2025-10-05 17:00 UTC))];
        assert_eq!(
            default_season_week(&games, Season(2025), datetime!(2025-10-05 23:00 UTC)),
            SeasonWeek {
                season: Season(2025),
                season_type: SeasonType::Reg,
                week: Week(1)
            }
        );
    }

    #[test]
    fn no_future_schedule_falls_back_to_configured_regular_week_one() {
        let games = vec![game("past", 5, datetime!(2025-10-05 17:00 UTC))];
        assert_eq!(
            default_season_week(&games, Season(2026), datetime!(2026-12-01 12:00 UTC)),
            SeasonWeek {
                season: Season(2026),
                season_type: SeasonType::Reg,
                week: Week(1)
            }
        );
    }
}
