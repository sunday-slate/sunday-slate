use nfl_data::{Game, SeasonType};
use time::{Date, Month, Time, Weekday};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Holiday {
    Thanksgiving,
    Christmas,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rule {
    Main { week: u8 },
    Holiday(Holiday),
    Playoff { week: u8 },
}

impl Rule {
    pub fn from_name(name: &str) -> Option<Rule> {
        match name {
            "Thanksgiving" => Some(Rule::Holiday(Holiday::Thanksgiving)),
            "Christmas" => Some(Rule::Holiday(Holiday::Christmas)),
            "Wild Card" => Some(Rule::Playoff { week: 19 }),
            "Divisional" => Some(Rule::Playoff { week: 20 }),
            "Championship" => Some(Rule::Playoff { week: 21 }),
            other => other
                .strip_prefix("Week ")
                .and_then(|n| n.parse::<u8>().ok())
                .filter(|w| (1..=18).contains(w))
                .map(|week| Rule::Main { week }),
        }
    }
}

fn fourth_thursday_of_november(year: i32) -> Date {
    let mut date = Date::from_calendar_date(year, Month::November, 1).expect("nov 1");
    let mut seen = 0;
    loop {
        if date.weekday() == Weekday::Thursday {
            seen += 1;
            if seen == 4 {
                return date;
            }
        }
        date = date.next_day().expect("within november");
    }
}

fn holiday_date(holiday: Holiday, season: u16) -> Date {
    let year = season as i32;
    match holiday {
        Holiday::Thanksgiving => fourth_thursday_of_november(year),
        Holiday::Christmas => Date::from_calendar_date(year, Month::December, 25).expect("dec 25"),
    }
}

/// Whether `game` belongs on `rule`'s slate. Pure: the single source of truth
/// for slate membership, for callers that only need the ids or the kickoffs.
pub fn covers(rule: &Rule, season: u16, game: &Game) -> bool {
    let noon = Time::from_hms(12, 0, 0).expect("noon");
    let six_pm = Time::from_hms(18, 0, 0).expect("6pm");
    let Some(et) = game.kickoff_eastern() else {
        return false;
    };
    match rule {
        Rule::Main { week } => {
            game.season_type == SeasonType::Reg
                && game.week.0 == *week
                && et.weekday() == Weekday::Sunday
                && et.time() >= noon
                && et.time() <= six_pm
        }
        Rule::Holiday(h) => et.date() == holiday_date(*h, season),
        Rule::Playoff { week } => {
            game.season_type == SeasonType::Post
                && game.week.0 == *week
                && matches!(et.weekday(), Weekday::Saturday | Weekday::Sunday)
        }
    }
}

/// The slate for `rule` over `games` (that season's nfl-data games).
pub fn resolve(rule: &Rule, season: u16, games: &[Game]) -> Vec<Game> {
    games
        .iter()
        .filter(|g| covers(rule, season, g))
        .cloned()
        .collect()
}

/// The NFL week a rule points at, as `(season_type, week)`. A holiday carries a
/// date rather than a week, so it resolves through the schedule; `None` when no
/// game falls on that date, because then nothing identifies the week.
fn candidate_week(rule: &Rule, season: u16, games: &[Game]) -> Option<(SeasonType, u8)> {
    match rule {
        Rule::Main { week } => Some((SeasonType::Reg, *week)),
        Rule::Playoff { week } => Some((SeasonType::Post, *week)),
        Rule::Holiday(h) => {
            let date = holiday_date(*h, season);
            games
                .iter()
                .find(|g| g.kickoff_eastern().is_some_and(|et| et.date() == date))
                .map(|g| (g.season_type, g.week.0))
        }
    }
}

/// Every game the commissioner may put on this contest's slate: the whole NFL
/// week the rule points at, chronological, kickoff-less games last. Wider than
/// `resolve`, which picks the default subset out of this pool.
pub fn candidates(rule: &Rule, season: u16, games: &[Game]) -> Vec<Game> {
    let Some((season_type, week)) = candidate_week(rule, season, games) else {
        return vec![];
    };
    let mut pool: Vec<Game> = games
        .iter()
        .filter(|g| g.season_type == season_type && g.week.0 == week)
        .cloned()
        .collect();
    pool.sort_by(|a, b| {
        (a.kickoff.is_none(), a.kickoff, &a.gsis_game_id).cmp(&(
            b.kickoff.is_none(),
            b.kickoff,
            &b.gsis_game_id,
        ))
    });
    pool
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_name_maps_every_canonical_name() {
        assert_eq!(Rule::from_name("Week 1"), Some(Rule::Main { week: 1 }));
        assert_eq!(Rule::from_name("Week 18"), Some(Rule::Main { week: 18 }));
        assert_eq!(
            Rule::from_name("Thanksgiving"),
            Some(Rule::Holiday(Holiday::Thanksgiving))
        );
        assert_eq!(
            Rule::from_name("Christmas"),
            Some(Rule::Holiday(Holiday::Christmas))
        );
        assert_eq!(
            Rule::from_name("Wild Card"),
            Some(Rule::Playoff { week: 19 })
        );
        assert_eq!(
            Rule::from_name("Divisional"),
            Some(Rule::Playoff { week: 20 })
        );
        assert_eq!(
            Rule::from_name("Championship"),
            Some(Rule::Playoff { week: 21 })
        );
    }

    #[test]
    fn from_name_rejects_junk_and_out_of_range_weeks() {
        assert_eq!(Rule::from_name("Week 0"), None);
        assert_eq!(Rule::from_name("Week 19"), None);
        assert_eq!(Rule::from_name("Preseason Week 1"), None);
        assert_eq!(Rule::from_name("nonsense"), None);
    }

    use nfl_data::{Season, SeasonType, TeamAbbr as NflTeamAbbr, Week};
    use time::OffsetDateTime;
    use time::macros::datetime;

    fn g(
        id: &str,
        week: u8,
        st: SeasonType,
        kickoff: OffsetDateTime,
        away: &str,
        home: &str,
    ) -> Game {
        Game {
            gsis_game_id: id.into(),
            season: Season(2025),
            week: Week(week),
            season_type: st,
            kickoff: Some(kickoff),
            away_team: NflTeamAbbr(away.into()),
            home_team: NflTeamAbbr(home.into()),
            home_score: None,
            away_score: None,
        }
    }

    fn ids(games: Vec<Game>) -> Vec<String> {
        let mut v: Vec<String> = games.into_iter().map(|g| g.gsis_game_id).collect();
        v.sort();
        v
    }

    #[test]
    fn main_slate_keeps_only_sunday_noon_to_six_et() {
        let games = vec![
            // Thursday night — excluded
            g(
                "thu",
                5,
                SeasonType::Reg,
                datetime!(2025-10-03 00:15 UTC),
                "SF",
                "LA",
            ),
            // Sunday 1:00pm ET (17:00Z EDT) — kept
            g(
                "sun_1pm",
                5,
                SeasonType::Reg,
                datetime!(2025-10-05 17:00 UTC),
                "BUF",
                "KC",
            ),
            // Sunday 4:25pm ET (20:25Z) — kept
            g(
                "sun_425",
                5,
                SeasonType::Reg,
                datetime!(2025-10-05 20:25 UTC),
                "NYG",
                "DAL",
            ),
            // Sunday night 8:20pm ET (00:20Z Mon) — excluded (> 18:00)
            g(
                "snf",
                5,
                SeasonType::Reg,
                datetime!(2025-10-06 00:20 UTC),
                "DET",
                "GB",
            ),
            // Wrong week — excluded
            g(
                "wk6",
                6,
                SeasonType::Reg,
                datetime!(2025-10-12 17:00 UTC),
                "MIA",
                "NYJ",
            ),
        ];
        assert_eq!(
            ids(resolve(&Rule::Main { week: 5 }, 2025, &games)),
            vec!["sun_1pm".to_string(), "sun_425".to_string()]
        );
    }

    #[test]
    fn holiday_keeps_all_games_on_the_et_date() {
        let games = vec![
            // Thanksgiving Thu 2025-11-27, three games (EST -5)
            g(
                "tg_1",
                13,
                SeasonType::Reg,
                datetime!(2025-11-27 18:00 UTC),
                "GB",
                "DET",
            ),
            g(
                "tg_2",
                13,
                SeasonType::Reg,
                datetime!(2025-11-27 21:30 UTC),
                "KC",
                "DAL",
            ),
            g(
                "tg_3",
                13,
                SeasonType::Reg,
                datetime!(2025-11-28 01:20 UTC),
                "CIN",
                "BAL",
            ),
            // Sunday of the same week — excluded from the holiday slate
            g(
                "sun",
                13,
                SeasonType::Reg,
                datetime!(2025-11-30 18:00 UTC),
                "SF",
                "LA",
            ),
        ];
        assert_eq!(
            ids(resolve(&Rule::Holiday(Holiday::Thanksgiving), 2025, &games)),
            vec!["tg_1".to_string(), "tg_2".to_string(), "tg_3".to_string()]
        );
    }

    #[test]
    fn playoff_keeps_saturday_and_sunday_but_not_monday() {
        let games = vec![
            // Wild Card, week 19: two Sat, one Mon (EST -5)
            g(
                "sat_1",
                19,
                SeasonType::Post,
                datetime!(2026-01-10 21:30 UTC),
                "LA",
                "CAR",
            ),
            g(
                "sun_1",
                19,
                SeasonType::Post,
                datetime!(2026-01-11 18:00 UTC),
                "BUF",
                "JAX",
            ),
            g(
                "mon_1",
                19,
                SeasonType::Post,
                datetime!(2026-01-13 01:00 UTC),
                "HOU",
                "PIT",
            ),
        ];
        assert_eq!(
            ids(resolve(&Rule::Playoff { week: 19 }, 2025, &games)),
            vec!["sat_1".to_string(), "sun_1".to_string()]
        );
    }

    #[test]
    fn candidates_for_a_main_rule_are_the_whole_regular_week() {
        let games = vec![
            g(
                "thu",
                5,
                SeasonType::Reg,
                datetime!(2025-10-03 00:15 UTC),
                "SF",
                "LA",
            ),
            g(
                "sun_1pm",
                5,
                SeasonType::Reg,
                datetime!(2025-10-05 17:00 UTC),
                "BUF",
                "KC",
            ),
            g(
                "snf",
                5,
                SeasonType::Reg,
                datetime!(2025-10-06 00:20 UTC),
                "DET",
                "GB",
            ),
            g(
                "wk6",
                6,
                SeasonType::Reg,
                datetime!(2025-10-12 17:00 UTC),
                "MIA",
                "NYJ",
            ),
        ];
        // Chronological, and unlike `resolve` it keeps Thursday and Sunday night.
        assert_eq!(
            candidates(&Rule::Main { week: 5 }, 2025, &games)
                .into_iter()
                .map(|g| g.gsis_game_id)
                .collect::<Vec<_>>(),
            vec!["thu".to_string(), "sun_1pm".to_string(), "snf".to_string()]
        );
    }

    #[test]
    fn candidates_for_a_playoff_rule_are_the_whole_post_week() {
        let games = vec![
            g(
                "sat_1",
                19,
                SeasonType::Post,
                datetime!(2026-01-10 21:30 UTC),
                "LA",
                "CAR",
            ),
            g(
                "mon_1",
                19,
                SeasonType::Post,
                datetime!(2026-01-13 01:00 UTC),
                "HOU",
                "PIT",
            ),
            g(
                "reg_19",
                19,
                SeasonType::Reg,
                datetime!(2026-01-11 18:00 UTC),
                "BUF",
                "JAX",
            ),
        ];
        // The Monday game is in the pool even though `resolve` drops it.
        assert_eq!(
            candidates(&Rule::Playoff { week: 19 }, 2025, &games)
                .into_iter()
                .map(|g| g.gsis_game_id)
                .collect::<Vec<_>>(),
            vec!["sat_1".to_string(), "mon_1".to_string()]
        );
    }

    #[test]
    fn candidates_for_a_holiday_are_the_week_the_holiday_falls_in() {
        let games = vec![
            g(
                "tg_1",
                13,
                SeasonType::Reg,
                datetime!(2025-11-27 18:00 UTC),
                "GB",
                "DET",
            ),
            g(
                "sun",
                13,
                SeasonType::Reg,
                datetime!(2025-11-30 18:00 UTC),
                "SF",
                "LA",
            ),
            g(
                "wk14",
                14,
                SeasonType::Reg,
                datetime!(2025-12-07 18:00 UTC),
                "MIA",
                "NYJ",
            ),
        ];
        // The holiday game fixes week 13; the whole week becomes the pool.
        assert_eq!(
            candidates(&Rule::Holiday(Holiday::Thanksgiving), 2025, &games)
                .into_iter()
                .map(|g| g.gsis_game_id)
                .collect::<Vec<_>>(),
            vec!["tg_1".to_string(), "sun".to_string()]
        );
    }

    #[test]
    fn candidates_are_empty_when_no_game_falls_on_the_holiday() {
        let games = vec![g(
            "sun",
            13,
            SeasonType::Reg,
            datetime!(2025-11-30 18:00 UTC),
            "SF",
            "LA",
        )];
        assert!(candidates(&Rule::Holiday(Holiday::Christmas), 2025, &games).is_empty());
    }
}
