//! Pure default membership policy for shared slates.
//!
//! The rule representation is internal: callers never construct or parse
//! rules. [`proposals`] generates the canonical definitions for an NFL week —
//! the Sunday main slate for regular weeks 1..=18, the named playoff slates
//! for postseason weeks 19..=21, and holiday slates when games in the exact
//! week fall on their Eastern holiday date. [`opening_monday`] derives the
//! Monday that opens the week from the games' known Eastern kickoff dates.

use time::{Date, Month, OffsetDateTime, Time, UtcOffset, Weekday};

use nfl_model::nfl_week_monday;

use crate::slates::SlateProposal;
use crate::{Game, SeasonType, SeasonWeek};

/// One canonical slate definition for a week. Internal: callers never
/// construct or parse rules; [`proposals`] generates them for the NFL week.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Rule {
    /// Regular-season weeks 1..=18: Sunday games kicking off noon–6pm Eastern.
    Main { week: u8 },
    /// Games on the fourth Thursday of November, Eastern date.
    Thanksgiving,
    /// Games on December 25, Eastern date.
    Christmas,
    /// Postseason week 19: Saturday and Sunday games.
    WildCard,
    /// Postseason week 20: Saturday and Sunday games.
    Divisional,
    /// Postseason week 21: Saturday and Sunday games.
    Championship,
}

impl Rule {
    /// The slate's canonical name, matching Sunday Slate's published names.
    fn name(self) -> String {
        match self {
            Rule::Main { week } => format!("Week {week}"),
            Rule::Thanksgiving => "Thanksgiving".into(),
            Rule::Christmas => "Christmas".into(),
            Rule::WildCard => "Wild Card".into(),
            Rule::Divisional => "Divisional".into(),
            Rule::Championship => "Championship".into(),
        }
    }

    /// Whether `game` belongs on this rule's slate: Eastern calendar dates,
    /// the Sunday noon–6pm window for main slates, the holiday date for
    /// holidays, and Saturday/Sunday for playoffs. Games with unknown
    /// kickoffs never match.
    fn covers(self, season: u16, game: &Game) -> bool {
        let noon = Time::from_hms(12, 0, 0).expect("noon");
        let six_pm = Time::from_hms(18, 0, 0).expect("6pm");
        let Some(et) = game.kickoff_eastern() else {
            return false;
        };
        match self {
            Rule::Main { week } => {
                game.season_type == SeasonType::Reg
                    && game.week.0 == week
                    && et.weekday() == Weekday::Sunday
                    && et.time() >= noon
                    && et.time() <= six_pm
            }
            Rule::Thanksgiving => et.date() == fourth_thursday_of_november(i32::from(season)),
            Rule::Christmas => et.date() == christmas_date(season),
            Rule::WildCard => playoff_week_covers(game, et, 19),
            Rule::Divisional => playoff_week_covers(game, et, 20),
            Rule::Championship => playoff_week_covers(game, et, 21),
        }
    }
}

/// The week's primary slate definition: the Sunday main slate for regular
/// weeks 1..=18, or the named playoff slate for postseason weeks 19..=21.
/// Weeks outside those ranges have no primary slate.
fn primary_rule(week: &SeasonWeek) -> Option<Rule> {
    match (week.season_type, week.week.0) {
        (SeasonType::Reg, week @ 1..=18) => Some(Rule::Main { week }),
        (SeasonType::Post, 19) => Some(Rule::WildCard),
        (SeasonType::Post, 20) => Some(Rule::Divisional),
        (SeasonType::Post, 21) => Some(Rule::Championship),
        _ => None,
    }
}

/// Every canonical slate proposal for `week`, in canonical order: the week's
/// main or playoff definition, then each holiday whose Eastern date has games
/// in the exact week. Games from other seasons, season types, or weeks never
/// contribute.
pub(crate) fn proposals(week: SeasonWeek, games: &[Game]) -> Vec<SlateProposal> {
    let week_games: Vec<&Game> = games.iter().filter(|g| in_week(g, &week)).collect();
    let season = week.season.0;
    let mut out = Vec::new();
    if let Some(primary) = primary_rule(&week) {
        out.push(proposal(primary, season, &week_games));
    }
    for holiday in [Rule::Thanksgiving, Rule::Christmas] {
        if week_games.iter().any(|g| holiday.covers(season, g)) {
            out.push(proposal(holiday, season, &week_games));
        }
    }
    out
}

/// The proposal for one rule over the week's games: the canonical name, the
/// earliest known selected kickoff normalized to UTC, and the selected ids.
fn proposal(rule: Rule, season: u16, week_games: &[&Game]) -> SlateProposal {
    let selected: Vec<&Game> = week_games
        .iter()
        .copied()
        .filter(|g| rule.covers(season, g))
        .collect();
    SlateProposal {
        name: rule.name(),
        kickoff: selected
            .iter()
            .filter_map(|g| g.kickoff)
            .map(|k| k.to_offset(UtcOffset::UTC))
            .min(),
        game_ids: selected.iter().map(|g| g.gsis_game_id.clone()).collect(),
    }
}

/// The Monday that opens `week`'s NFL schedule window: the earliest known
/// Eastern kickoff date among all of the exact week's games — including games
/// outside the main slate — mapped through `nfl_week_monday` (Monday Night
/// Football belongs to the preceding NFL week). `None` when no game in the
/// week has a known kickoff.
pub(crate) fn opening_monday(week: SeasonWeek, games: &[Game]) -> Option<Date> {
    games
        .iter()
        .filter(|g| in_week(g, &week))
        .filter_map(|g| g.kickoff_eastern().map(|et| et.date()))
        .min()
        .map(nfl_week_monday)
}

/// Whether `game` belongs to the exact NFL week, regardless of any slate rule.
fn in_week(game: &Game, week: &SeasonWeek) -> bool {
    game.season == week.season && game.season_type == week.season_type && game.week == week.week
}

/// Playoff membership: the rule's postseason week, Saturday and Sunday only.
fn playoff_week_covers(game: &Game, et: OffsetDateTime, week: u8) -> bool {
    game.season_type == SeasonType::Post
        && game.week.0 == week
        && matches!(et.weekday(), Weekday::Saturday | Weekday::Sunday)
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

fn christmas_date(season: u16) -> Date {
    Date::from_calendar_date(i32::from(season), Month::December, 25).expect("dec 25")
}

#[cfg(test)]
mod tests {
    use time::OffsetDateTime;
    use time::UtcOffset;
    use time::macros::{date, datetime};

    use super::{opening_monday, proposals};
    use crate::slates::SlateProposal;
    use crate::{Game, Season, SeasonType, SeasonWeek, Slate, SlateId, Week};

    /// A fixture game with irrelevant teams and no scores: membership depends
    /// only on the season week and the kickoff's Eastern time.
    fn game(
        id: &str,
        season: u16,
        week: u8,
        season_type: SeasonType,
        kickoff: Option<OffsetDateTime>,
    ) -> Game {
        Game {
            gsis_game_id: id.into(),
            season: Season(season),
            week: Week(week),
            season_type,
            kickoff,
            away_team: "KC".parse().unwrap(),
            home_team: "BUF".parse().unwrap(),
            home_score: None,
            away_score: None,
        }
    }

    fn reg_week(season: u16, week: u8) -> SeasonWeek {
        SeasonWeek {
            season: Season(season),
            season_type: SeasonType::Reg,
            week: Week(week),
        }
    }

    fn post_week(season: u16, week: u8) -> SeasonWeek {
        SeasonWeek {
            season: Season(season),
            season_type: SeasonType::Post,
            week: Week(week),
        }
    }

    fn proposal_named<'a>(all: &'a [SlateProposal], name: &str) -> &'a SlateProposal {
        all.iter()
            .find(|p| p.name == name)
            .unwrap_or_else(|| panic!("no proposal named {name} in {all:?}"))
    }

    /// Membership, independent of the unspecified game-id ordering.
    fn sorted_ids(proposal: &SlateProposal) -> Vec<&str> {
        let mut ids: Vec<&str> = proposal.game_ids.iter().map(String::as_str).collect();
        ids.sort_unstable();
        ids
    }

    #[test]
    fn main_rule_matches_existing_eastern_window() {
        // October 2025 is EDT (UTC-4), so noon ET is 16:00 UTC.
        let games = vec![
            // Thursday night — outside the Sunday window.
            game(
                "thu",
                2025,
                5,
                SeasonType::Reg,
                Some(datetime!(2025-10-03 00:15 UTC)),
            ),
            // Sunday 11:59 ET — one minute before the window.
            game(
                "sun_1159",
                2025,
                5,
                SeasonType::Reg,
                Some(datetime!(2025-10-05 15:59 UTC)),
            ),
            // Sunday 12:00 ET — the lower boundary is inclusive. The fixture
            // carries the Eastern-local offset the schedule still uses; the
            // proposal must normalize it to UTC.
            game(
                "sun_1200",
                2025,
                5,
                SeasonType::Reg,
                Some(datetime!(2025-10-05 12:00 -04)),
            ),
            // Sunday 4:25pm ET — an ordinary afternoon game.
            game(
                "sun_1625",
                2025,
                5,
                SeasonType::Reg,
                Some(datetime!(2025-10-05 20:25 UTC)),
            ),
            // Sunday 18:00 ET — the upper boundary is inclusive.
            game(
                "sun_1800",
                2025,
                5,
                SeasonType::Reg,
                Some(datetime!(2025-10-05 22:00 UTC)),
            ),
            // Sunday 18:01 ET — one minute past the window.
            game(
                "sun_1801",
                2025,
                5,
                SeasonType::Reg,
                Some(datetime!(2025-10-05 22:01 UTC)),
            ),
            // Sunday night — after 6pm ET.
            game(
                "snf",
                2025,
                5,
                SeasonType::Reg,
                Some(datetime!(2025-10-06 00:20 UTC)),
            ),
            // A different week — outside the requested season week entirely.
            game(
                "wk6",
                2025,
                6,
                SeasonType::Reg,
                Some(datetime!(2025-10-12 17:00 UTC)),
            ),
        ];

        let got = proposals(reg_week(2025, 5), &games);
        assert_eq!(got.len(), 1, "an ordinary week has only its main slate");
        let main = proposal_named(&got, "Week 5");
        assert_eq!(sorted_ids(main), ["sun_1200", "sun_1625", "sun_1800"]);
        assert_eq!(main.kickoff, Some(datetime!(2025-10-05 16:00 UTC)));
        assert_eq!(
            main.kickoff.unwrap().offset(),
            UtcOffset::UTC,
            "the proposal kickoff is normalized to UTC"
        );
    }

    #[test]
    fn holiday_rules_use_eastern_dates() {
        // Thanksgiving is the fourth Thursday of November 2025 (EST, UTC-5):
        // the night game at 2025-11-28 01:20 UTC is still Thursday locally.
        let thanksgiving = vec![
            game(
                "tg_1",
                2025,
                13,
                SeasonType::Reg,
                Some(datetime!(2025-11-27 18:00 UTC)),
            ),
            game(
                "tg_2",
                2025,
                13,
                SeasonType::Reg,
                Some(datetime!(2025-11-27 21:30 UTC)),
            ),
            game(
                "tg_3",
                2025,
                13,
                SeasonType::Reg,
                Some(datetime!(2025-11-28 01:20 UTC)),
            ),
            game(
                "sun",
                2025,
                13,
                SeasonType::Reg,
                Some(datetime!(2025-11-30 18:00 UTC)),
            ),
        ];
        let got = proposals(reg_week(2025, 13), &thanksgiving);
        assert_eq!(
            got.len(),
            2,
            "Thanksgiving week has its main and holiday slates"
        );
        let holiday = proposal_named(&got, "Thanksgiving");
        assert_eq!(sorted_ids(holiday), ["tg_1", "tg_2", "tg_3"]);
        assert_eq!(holiday.kickoff, Some(datetime!(2025-11-27 18:00 UTC)));

        // Christmas uses December 25 in Eastern local time: 2025-12-26 01:15
        // UTC is still Christmas Day locally, while 06:00 UTC already is not.
        let christmas = vec![
            game(
                "xmas_early",
                2025,
                16,
                SeasonType::Reg,
                Some(datetime!(2025-12-25 18:00 UTC)),
            ),
            game(
                "xmas_late",
                2025,
                16,
                SeasonType::Reg,
                Some(datetime!(2025-12-26 01:15 UTC)),
            ),
            game(
                "xmas_over",
                2025,
                16,
                SeasonType::Reg,
                Some(datetime!(2025-12-26 06:00 UTC)),
            ),
            game(
                "sun",
                2025,
                16,
                SeasonType::Reg,
                Some(datetime!(2025-12-28 18:00 UTC)),
            ),
        ];
        let got = proposals(reg_week(2025, 16), &christmas);
        assert_eq!(
            got.len(),
            2,
            "Christmas week has its main and holiday slates"
        );
        let holiday = proposal_named(&got, "Christmas");
        assert_eq!(sorted_ids(holiday), ["xmas_early", "xmas_late"]);
        assert_eq!(holiday.kickoff, Some(datetime!(2025-12-25 18:00 UTC)));
    }

    #[test]
    fn playoff_rules_exclude_monday() {
        // Postseason weeks are EST (UTC-5): Saturday 21:30 UTC is 4:30pm ET,
        // and each Monday night kickoff lands on Monday locally.
        let cases = [
            (
                (19u8, "Wild Card"),
                datetime!(2026-01-10 21:30 UTC),
                datetime!(2026-01-11 18:00 UTC),
                datetime!(2026-01-13 01:00 UTC),
            ),
            (
                (20, "Divisional"),
                datetime!(2026-01-17 21:30 UTC),
                datetime!(2026-01-18 18:00 UTC),
                datetime!(2026-01-20 01:00 UTC),
            ),
            (
                (21, "Championship"),
                datetime!(2026-01-24 21:30 UTC),
                datetime!(2026-01-25 18:00 UTC),
                datetime!(2026-01-27 01:00 UTC),
            ),
        ];
        for ((week, name), saturday, sunday, monday) in cases {
            let games = vec![
                game("sat", 2025, week, SeasonType::Post, Some(saturday)),
                game("sun", 2025, week, SeasonType::Post, Some(sunday)),
                game("mon", 2025, week, SeasonType::Post, Some(monday)),
            ];

            let got = proposals(post_week(2025, week), &games);
            assert_eq!(got.len(), 1, "postseason week {week} has exactly one slate");
            let playoff = proposal_named(&got, name);
            assert_eq!(playoff.name, name);
            assert_eq!(sorted_ids(playoff), ["sat", "sun"]);
            assert_eq!(playoff.kickoff, Some(saturday));
        }
    }

    #[test]
    fn proposals_are_scoped_to_exact_season_week() {
        let games = vec![
            // 2025 REG 13 — the only game for the requested week.
            game(
                "own_week",
                2025,
                13,
                SeasonType::Reg,
                Some(datetime!(2025-11-30 18:00 UTC)),
            ),
            // Same numeric week, previous season.
            game(
                "other_season",
                2024,
                13,
                SeasonType::Reg,
                Some(datetime!(2024-12-01 18:00 UTC)),
            ),
            // Same numeric week, postseason type.
            game(
                "other_type",
                2025,
                13,
                SeasonType::Post,
                Some(datetime!(2026-01-04 18:00 UTC)),
            ),
        ];

        let got = proposals(reg_week(2025, 13), &games);
        let main = proposal_named(&got, "Week 13");
        assert_eq!(sorted_ids(main), ["own_week"]);
        assert_eq!(
            main.kickoff,
            Some(datetime!(2025-11-30 18:00 UTC)),
            "foreign games contribute neither games nor kickoffs"
        );

        // The previous season's week 13 resolves only its own game.
        let got = proposals(reg_week(2024, 13), &games);
        assert_eq!(
            sorted_ids(proposal_named(&got, "Week 13")),
            ["other_season"]
        );
    }

    #[test]
    fn holiday_and_main_share_a_week() {
        let games = vec![
            game(
                "tg_1",
                2025,
                13,
                SeasonType::Reg,
                Some(datetime!(2025-11-27 18:00 UTC)),
            ),
            game(
                "tg_2",
                2025,
                13,
                SeasonType::Reg,
                Some(datetime!(2025-11-27 21:30 UTC)),
            ),
            game(
                "tg_3",
                2025,
                13,
                SeasonType::Reg,
                Some(datetime!(2025-11-28 01:20 UTC)),
            ),
            game(
                "sun_1200",
                2025,
                13,
                SeasonType::Reg,
                Some(datetime!(2025-11-30 17:00 UTC)),
            ),
            game(
                "sun_425",
                2025,
                13,
                SeasonType::Reg,
                Some(datetime!(2025-11-30 20:25 UTC)),
            ),
        ];

        let got = proposals(reg_week(2025, 13), &games);
        assert_eq!(
            got.len(),
            2,
            "the holiday and the main slate share the week"
        );
        let holiday = proposal_named(&got, "Thanksgiving");
        let main = proposal_named(&got, "Week 13");
        assert_eq!(sorted_ids(holiday), ["tg_1", "tg_2", "tg_3"]);
        assert_eq!(sorted_ids(main), ["sun_1200", "sun_425"]);
        assert_eq!(holiday.kickoff, Some(datetime!(2025-11-27 18:00 UTC)));
        assert_eq!(main.kickoff, Some(datetime!(2025-11-30 17:00 UTC)));

        // The slates share the week but not a single game.
        for id in &holiday.game_ids {
            assert!(!main.game_ids.contains(id), "{id} belongs to both slates");
        }
    }

    #[test]
    fn opening_monday_uses_game_dates_not_the_clock() {
        let week = reg_week(2025, 1);
        // The Thursday opener, a Sunday game, and MNF, all within NFL week 1.
        // Monday kickoffs belong to the preceding NFL week, so every known
        // game date maps to the same opening Monday.
        let thursday = game(
            "thu",
            2025,
            1,
            SeasonType::Reg,
            Some(datetime!(2025-09-05 00:15 UTC)),
        );
        let sunday = game(
            "sun",
            2025,
            1,
            SeasonType::Reg,
            Some(datetime!(2025-09-07 17:00 UTC)),
        );
        let mnf = game(
            "mnf",
            2025,
            1,
            SeasonType::Reg,
            Some(datetime!(2025-09-09 00:15 UTC)),
        );
        let unknown = game("unknown", 2025, 1, SeasonType::Reg, None);
        // The previous season's week 1 must not move the opening Monday.
        let other_season = game(
            "other_season",
            2024,
            1,
            SeasonType::Reg,
            Some(datetime!(2024-09-09 17:00 UTC)),
        );

        assert_eq!(
            opening_monday(week, std::slice::from_ref(&sunday)),
            Some(date!(2025 - 09 - 01))
        );
        assert_eq!(
            opening_monday(week, std::slice::from_ref(&mnf)),
            Some(date!(2025 - 09 - 01))
        );
        assert_eq!(
            opening_monday(week, &[thursday, sunday, mnf, unknown, other_season]),
            Some(date!(2025 - 09 - 01))
        );
    }

    #[test]
    fn proposals_generate_the_primary_slate_even_without_games() {
        // The week's primary definition exists before any game is known; only
        // its membership waits for the schedule.
        let got = proposals(reg_week(2025, 5), &[]);
        assert_eq!(got.len(), 1);
        let main = &got[0];
        assert_eq!(main.name, "Week 5");
        assert!(main.game_ids.is_empty());
        assert_eq!(main.kickoff, None);

        let got = proposals(post_week(2025, 19), &[]);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].name, "Wild Card");
        assert!(got[0].game_ids.is_empty());
        assert_eq!(got[0].kickoff, None);

        // Weeks outside the canonical ranges have no primary slate.
        assert_eq!(proposals(post_week(2025, 22), &[]).len(), 0);
    }

    #[test]
    fn opening_monday_returns_none_without_known_kickoffs() {
        let week = reg_week(2025, 1);
        assert_eq!(opening_monday(week, &[]), None);
        assert_eq!(
            opening_monday(week, &[game("unknown", 2025, 1, SeasonType::Reg, None)]),
            None
        );
    }

    #[test]
    fn slate_metadata_constructs_with_the_four_public_fields() {
        let week = reg_week(2025, 1);
        let slate = Slate {
            id: SlateId(1),
            season_week: week,
            name: "Week 1".into(),
            kickoff: None,
        };
        assert_eq!(slate.kickoff, None);
        assert_eq!(slate.id, SlateId(1));
        assert_eq!(slate.season_week, week);
        assert_eq!(slate.name, "Week 1");
    }
}
