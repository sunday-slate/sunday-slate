//! Contest lookups composing the store with NFL schedule data, plus pure contest-list assembly.

use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};

use nfl_data::{Game, NflData, Season, Week};
use sqlx::Sqlite;
use time::{Date, OffsetDateTime};

use crate::contests::{Contest, ContestId, NflGameId, published_slate, store};
use crate::entries::store as entries_store;
use crate::entries::{ContestEntry, EntryId};
use crate::fantasy_teams::store as fantasy_teams_store;
use crate::fantasy_teams::{FantasyTeamId, LeagueTeam, monogram};
use crate::live::{self, Coverage, LINEUP_REGULATION_MINUTES, LiveContestSnapshot};
use crate::scoring::{self, WeekStats, score_lineup};
use crate::standings::standings::{self, ScoredFantasyTeam};
use crate::{AppError, AppState};

/// The season schedule and every contest's frozen slate, fetched together.
///
/// Contests resolve against the schedule in four places: the home
/// dispatcher, the contest list, the standings, and the team detail page.
/// Each one loads this pair once per request and folds it with the pure
/// methods below.
pub struct SeasonSlates {
    pub games: Vec<Game>,
    pub slates: HashMap<ContestId, Vec<NflGameId>>,
}

impl SeasonSlates {
    pub async fn load<'e, E>(ex: E, nfl: &NflData, season: Season) -> Result<Self, AppError>
    where
        E: sqlx::Executor<'e, Database = Sqlite>,
    {
        let games = nfl.games(season).await?;
        let slates = store::materialized_games(ex).await?;
        Ok(Self { games, slates })
    }

    /// Each materialized contest's NFL week. A contest whose games are absent
    /// from the schedule resolves to no week.
    pub fn contest_weeks(&self) -> HashMap<ContestId, Week> {
        let game_week: HashMap<&str, Week> = self
            .games
            .iter()
            .map(|g| (g.gsis_game_id.as_str(), g.week))
            .collect();
        self.slates
            .iter()
            .filter_map(|(contest, ids)| {
                let week = ids
                    .iter()
                    .find_map(|id| game_week.get(id.0.as_str()).copied())?;
                Some((*contest, week))
            })
            .collect()
    }

    /// Each finished materialized contest's NFL week.
    pub fn finished_contest_weeks(&self, now: OffsetDateTime) -> HashMap<ContestId, Week> {
        let mut contest_week = self.contest_weeks();
        let kickoffs = self.kickoffs();
        contest_week.retain(|contest, _| {
            let ids = &self.slates[contest];
            let first = ids
                .iter()
                .filter_map(|id| kickoffs.get(id.0.as_str()).copied())
                .min();
            let last = ids
                .iter()
                .filter_map(|id| kickoffs.get(id.0.as_str()).copied())
                .max();
            contest_state(first, last, now) == ContestState::Finished
        });
        contest_week
    }

    /// Each scheduled game's Eastern kickoff, keyed by gsis id.
    pub(crate) fn kickoffs(&self) -> HashMap<&str, OffsetDateTime> {
        self.games
            .iter()
            .filter_map(|g| Some((g.gsis_game_id.as_str(), g.kickoff_eastern()?)))
            .collect()
    }

    /// Pair each contest with its slate's earliest and latest kickoff.
    /// Contests with no frozen slate, or no game in the current schedule, are dropped.
    pub fn slate_contests(&self, contests: &[Contest]) -> Vec<SlateContest> {
        // Present as a key means the game is on the schedule; the value is
        // its kickoff, which a scheduled game may still be missing.
        let scheduled: HashMap<&str, Option<OffsetDateTime>> = self
            .games
            .iter()
            .map(|g| (g.gsis_game_id.as_str(), g.kickoff_eastern()))
            .collect();
        contests
            .iter()
            .filter_map(|c| {
                let ids = self.slates.get(&c.id)?;
                if !ids.iter().any(|id| scheduled.contains_key(id.0.as_str())) {
                    return None;
                }
                let kicks: Vec<OffsetDateTime> = ids
                    .iter()
                    .filter_map(|id| scheduled.get(id.0.as_str()).copied().flatten())
                    .collect();
                Some(SlateContest {
                    id: c.id,
                    name: c.name.clone(),
                    first: kicks.iter().min().copied(),
                    last: kicks.iter().max().copied(),
                })
            })
            .collect()
    }
}

/// A slate-set contest heading into list assembly: name plus the slate's
/// first and last kickoff (None when no game in the slate has a kickoff).
pub struct SlateContest {
    pub id: ContestId,
    pub name: String,
    pub first: Option<OffsetDateTime>,
    pub last: Option<OffsetDateTime>,
}

/// A contest's winning fantasy team, for display.
pub struct WinnerRow {
    pub name: String,
    pub owner_name: String,
    pub points: f64,
}

impl WinnerRow {
    pub fn points_str(&self) -> String {
        scoring::format_points(self.points)
    }
}

/// Where a contest sits in time, decided from its slate and the clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContestState {
    Upcoming,
    Live,
    Finished,
}

/// An upcoming contest's kickoff, as the middle column's headline and
/// sub-line: "Sunday" over "1:00pm ET".
pub struct Kickoff {
    pub weekday: String,
    pub time: String,
}

impl Kickoff {
    /// Rendered in US Eastern wall-clock time. Converting the instant keeps
    /// this correct for inputs with any stored offset.
    fn new(kickoff: OffsetDateTime) -> Self {
        use time::macros::format_description;
        let k = nfl_data::to_eastern(kickoff);
        let time = k
            .format(format_description!(
                "[hour repr:12 padding:none]:[minute][period case:lower]"
            ))
            .expect("kickoff carries a time");
        Self {
            weekday: k
                .format(format_description!("[weekday]"))
                .expect("kickoff carries a weekday"),
            time: format!("{time} ET"),
        }
    }
}

/// One rendered contest-list row.
pub struct ContestRow {
    /// The `/contests/{id}` link target.
    pub id: ContestId,
    pub name: String,
    pub dates: String,
    /// Upcoming rows only.
    pub kickoff: Option<Kickoff>,
    pub entry_count: usize,
    pub winners: Vec<WinnerRow>,
    pub state: ContestState,
    pub pending_official: bool,
}
impl ContestRow {
    pub fn is_live(&self) -> bool {
        self.state == ContestState::Live
    }

    pub fn is_finished(&self) -> bool {
        self.state == ContestState::Finished
    }
}

/// A published slate is read-only once its earliest kickoff has passed. The
/// gate protects lineups, and lineups exist only against a published slate, so
/// an unpublished contest is never locked.
pub fn slate_locked(published: &[Game], now: OffsetDateTime) -> bool {
    published
        .iter()
        .filter_map(Game::kickoff_eastern)
        .min()
        .is_some_and(|kickoff| now >= kickoff)
}

/// The Monday opening the NFL week that holds `kickoff`. Tue–Sun kickoffs
/// (Thursday openers, Saturday playoff slates, Sunday main slates) belong to
/// the week of the preceding Monday; a Monday kickoff is that week's finale
/// (Monday Night Football), so it belongs to the week that opened seven days
/// earlier.
pub(crate) fn nfl_week_monday(kickoff: Date) -> Date {
    let days_into_week = match kickoff.weekday().number_days_from_monday() {
        0 => 7,
        n => i64::from(n),
    };
    kickoff - time::Duration::days(days_into_week)
}

/// Classify a contest from its slate's first and last kickoff and the current
/// instant. A slate with no known kickoff has not started.
pub(crate) fn contest_state(
    first: Option<OffsetDateTime>,
    last: Option<OffsetDateTime>,
    now: OffsetDateTime,
) -> ContestState {
    let (Some(first), Some(last)) = (first, last) else {
        return ContestState::Upcoming;
    };
    if now.date() > last.date() {
        ContestState::Finished
    } else if now >= first {
        ContestState::Live
    } else {
        ContestState::Upcoming
    }
}

/// The winners of a finished contest: every entered team tied at the top
/// score. Empty when no entry scored above zero, which also covers a contest
/// with no entries at all.
fn winners(contest: ContestId, teams: &[ScoredFantasyTeam]) -> Vec<WinnerRow> {
    let scored: Vec<(&ScoredFantasyTeam, f64)> = teams
        .iter()
        .filter_map(|t| {
            let (_, points) = t.scores.iter().find(|(cid, _)| *cid == contest)?;
            Some((t, *points))
        })
        .collect();
    let Some(top) = scoring::winning_score(scored.iter().map(|(_, p)| *p)) else {
        return Vec::new();
    };
    scored
        .iter()
        .filter(|(_, p)| scoring::ties(*p, top))
        .map(|(t, p)| WinnerRow {
            name: t.name.clone(),
            owner_name: t.owner_name.clone(),
            points: *p,
        })
        .collect()
}

/// Fold contests and scored fantasy teams into display rows, newest first.
/// Winners show only once a contest has finished.
pub fn contest_list(
    contests: &[SlateContest],
    teams: &[ScoredFantasyTeam],
    now: OffsetDateTime,
) -> Vec<ContestRow> {
    let mut dated: Vec<(Option<OffsetDateTime>, ContestRow)> = contests
        .iter()
        .map(|c| {
            let state = contest_state(c.first, c.last, now);
            let row = ContestRow {
                id: c.id,
                name: c.name.clone(),
                dates: match (c.first, c.last) {
                    (Some(f), Some(l)) => format_dates(f.date(), l.date()),
                    _ => String::new(),
                },
                kickoff: match (state, c.first) {
                    (ContestState::Upcoming, Some(k)) => Some(Kickoff::new(k)),
                    _ => None,
                },
                entry_count: teams.iter().filter(|t| t.entered.contains(&c.id)).count(),
                winners: match state {
                    ContestState::Finished => winners(c.id, teams),
                    _ => Vec::new(),
                },
                state,
                pending_official: false,
            };
            (c.first, row)
        })
        .collect();

    // Newest first; contests with no resolvable kickoff sort last (stable).
    dated.sort_by_key(|(first, _)| Reverse(*first));
    dated.into_iter().map(|(_, row)| row).collect()
}

/// "Sep 7" for one day, "Jan 10–11" within a month, "Nov 30 – Dec 1" across.
fn format_dates(first: time::Date, last: time::Date) -> String {
    use time::macros::format_description;
    let month_day = format_description!("[month repr:short] [day padding:none]");
    let day = format_description!("[day padding:none]");
    // Both descriptions ask only for components a `Date` carries, so
    // formatting cannot fail.
    let fmt = |d: time::Date, desc: &[time::format_description::BorrowedFormatItem<'_>]| {
        d.format(desc).expect("date components only")
    };
    let f = fmt(first, month_day);
    if first == last {
        f
    } else if first.month() == last.month() {
        format!("{f}–{}", fmt(last, day))
    } else {
        format!("{f} – {}", fmt(last, month_day))
    }
}

#[cfg(test)]
mod contest_list_tests {
    use super::*;
    use time::macros::{date, datetime};

    /// A fixed "now" late enough that every Sept 2025 contest is finished.
    fn now() -> OffsetDateTime {
        datetime!(2026 - 01 - 01 0:00 UTC)
    }

    /// A 1:00pm Eastern kickoff on `date`, the shape the handler feeds in.
    fn kickoff(date: time::Date) -> OffsetDateTime {
        date.with_hms(13, 0, 0)
            .expect("valid time")
            .assume_offset(nfl_data::eastern_offset(date))
    }

    fn contest(
        id: i64,
        name: &str,
        first: Option<time::Date>,
        last: Option<time::Date>,
    ) -> SlateContest {
        SlateContest {
            id: ContestId(id),
            name: name.into(),
            first: first.map(kickoff),
            last: last.map(kickoff),
        }
    }

    fn team(name: &str, entered: &[i64], scores: &[(i64, f64)]) -> ScoredFantasyTeam {
        ScoredFantasyTeam {
            id: FantasyTeamId(0),
            name: name.into(),
            owner_name: "Owner".into(),
            logo: None,
            entered: entered.iter().map(|c| ContestId(*c)).collect(),
            scores: scores.iter().map(|(c, p)| (ContestId(*c), *p)).collect(),
        }
    }

    fn one_day(id: i64, name: &str, day: time::Date) -> SlateContest {
        contest(id, name, Some(day), Some(day))
    }

    #[test]
    fn upcoming_contest_shows_kickoff_and_no_winner() {
        let rows = contest_list(
            &[one_day(1, "Week 2", date!(2025 - 09 - 14))],
            &[team("Alpha", &[1], &[(1, 13.0)])],
            datetime!(2025 - 09 - 10 12:00 UTC),
        );
        assert_eq!(rows[0].state, ContestState::Upcoming);
        assert!(rows[0].winners.is_empty(), "upcoming shows no winner");
        let kickoff = rows[0].kickoff.as_ref().expect("kickoff shown");
        assert_eq!(kickoff.weekday, "Sunday");
        assert_eq!(kickoff.time, "1:00pm ET");
    }

    #[test]
    fn live_contest_has_no_winner() {
        let rows = contest_list(
            &[one_day(1, "Week 2", date!(2025 - 09 - 14))],
            &[team("Alpha", &[1], &[(1, 13.0)])],
            datetime!(2025 - 09 - 14 20:00 UTC),
        );
        assert!(rows[0].is_live());
        assert!(rows[0].winners.is_empty(), "live shows no winner");
        assert!(rows[0].kickoff.is_none());
    }

    #[test]
    fn a_contest_stays_live_until_its_last_gameday_is_over() {
        // A Sunday-to-Monday slate is still live during the Monday game.
        let rows = contest_list(
            &[contest(
                1,
                "Week 2",
                Some(date!(2025 - 09 - 14)),
                Some(date!(2025 - 09 - 15)),
            )],
            &[team("Alpha", &[1], &[(1, 13.0)])],
            datetime!(2025 - 09 - 15 22:00 -4),
        );
        assert!(rows[0].is_live());
    }

    #[test]
    fn finished_contest_shows_winner() {
        let rows = contest_list(
            &[one_day(1, "Week 1", date!(2025 - 09 - 07))],
            &[team("Alpha", &[1], &[(1, 13.0)])],
            datetime!(2025 - 09 - 09 12:00 UTC),
        );
        assert!(rows[0].is_finished());
        assert_eq!(rows[0].winners.len(), 1);
        assert_eq!(rows[0].winners[0].name, "Alpha");
    }

    #[test]
    fn winner_is_top_score_with_entry_count() {
        let rows = contest_list(
            &[one_day(1, "Week 1", date!(2025 - 09 - 07))],
            &[
                team("Alpha", &[1], &[(1, 13.0)]),
                team("Bravo", &[1], &[(1, 5.0)]),
            ],
            now(),
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "Week 1");
        assert_eq!(rows[0].entry_count, 2);
        assert_eq!(rows[0].winners.len(), 1);
        assert_eq!(rows[0].winners[0].name, "Alpha");
        assert_eq!(rows[0].winners[0].points, 13.0);
    }

    #[test]
    fn tied_top_scores_all_win() {
        let rows = contest_list(
            &[one_day(1, "Week 1", date!(2025 - 09 - 07))],
            &[
                team("Alpha", &[1], &[(1, 50.0)]),
                team("Bravo", &[1], &[(1, 50.0)]),
                team("Charlie", &[1], &[(1, 10.0)]),
            ],
            now(),
        );
        let names: Vec<&str> = rows[0].winners.iter().map(|w| w.name.as_str()).collect();
        assert_eq!(names, vec!["Alpha", "Bravo"]);
    }

    #[test]
    fn winners_survive_a_sub_cent_scoring_difference() {
        // Two identical lineups whose sums land a bit apart must both win.
        let rows = contest_list(
            &[one_day(1, "Week 1", date!(2025 - 09 - 07))],
            &[
                team("Alpha", &[1], &[(1, 50.0)]),
                team("Bravo", &[1], &[(1, 50.0 + 1e-12)]),
            ],
            now(),
        );
        let names: Vec<&str> = rows[0].winners.iter().map(|w| w.name.as_str()).collect();
        assert_eq!(names, vec!["Alpha", "Bravo"]);
    }

    #[test]
    fn zero_top_score_suppresses_winners() {
        let rows = contest_list(
            &[one_day(1, "Week 1", date!(2025 - 09 - 14))],
            &[
                team("Alpha", &[1], &[(1, 0.0)]),
                team("Bravo", &[1], &[(1, 0.0)]),
            ],
            now(),
        );
        assert!(rows[0].winners.is_empty(), "no 0-point winner");
        assert_eq!(rows[0].entry_count, 2);
    }

    #[test]
    fn contest_without_entries_is_schedule_only() {
        let rows = contest_list(
            &[one_day(1, "Week 2", date!(2025 - 09 - 14))],
            &[team("Alpha", &[], &[])],
            now(),
        );
        assert_eq!(rows[0].entry_count, 0);
        assert!(rows[0].winners.is_empty());
    }

    #[test]
    fn sorts_newest_first_undated_last() {
        let rows = contest_list(
            &[
                one_day(1, "Week 1", date!(2025 - 09 - 07)),
                one_day(2, "Week 2", date!(2025 - 09 - 14)),
                contest(3, "Mystery", None, None),
            ],
            &[],
            now(),
        );
        let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["Week 2", "Week 1", "Mystery"]);
    }

    #[test]
    fn date_display_single_day_and_ranges() {
        let rows = contest_list(
            &[
                one_day(1, "Single", date!(2025 - 09 - 07)),
                contest(
                    2,
                    "SameMonth",
                    Some(date!(2026 - 01 - 10)),
                    Some(date!(2026 - 01 - 11)),
                ),
                contest(
                    3,
                    "CrossMonth",
                    Some(date!(2025 - 11 - 30)),
                    Some(date!(2025 - 12 - 01)),
                ),
                contest(4, "NoDate", None, None),
            ],
            &[],
            now(),
        );
        let by_name = |n: &str| rows.iter().find(|r| r.name == n).unwrap();
        assert_eq!(by_name("Single").dates, "Sep 7");
        assert_eq!(by_name("SameMonth").dates, "Jan 10–11");
        assert_eq!(by_name("CrossMonth").dates, "Nov 30 – Dec 1");
        assert_eq!(by_name("NoDate").dates, "");
    }

    #[test]
    fn winner_row_formatting() {
        let w = WinnerRow {
            name: "Gridiron Giants".into(),
            owner_name: "Mike".into(),
            points: 142.5,
        };
        assert_eq!(w.points_str(), "142.50");
    }
}

/// A league fantasy team entered in one contest. `points` is `Some` when the
/// contest's NFL week resolves.
pub struct ContestEntrant {
    pub id: EntryId,
    pub name: String,
    pub owner_name: String,
    pub logo: Option<String>,
    pub points: Option<f64>,
}

/// A rendered contest entry row. Pre-results, `rank` and `points` are `None`
/// and `winner` is false on every row.
pub struct EntryRow {
    /// The `/entries/{id}` link target.
    pub id: EntryId,
    pub rank: Option<u32>,
    pub name: String,
    pub owner_name: String,
    pub logo: Option<String>,
    pub points: Option<f64>,
    pub coverage: Coverage,
    pub winner: bool,
    /// The viewer's own entry, highlighted in every contest state; pinned first
    /// and linked while the slate is open.
    pub mine: bool,
    /// The entry's remaining regulation game-clock minutes, `Some` only on the
    /// Provisional path; the official and pre-kickoff ranks keep it `None`.
    pub minutes_remaining: Option<f64>,
}

impl EntryRow {
    /// An entrant as an unranked, unscored row — the shared base for both
    /// list states.
    fn unscored(e: ContestEntrant) -> EntryRow {
        EntryRow {
            id: e.id,
            rank: None,
            name: e.name,
            owner_name: e.owner_name,
            logo: e.logo,
            points: None,
            coverage: Coverage::Unavailable,
            winner: false,
            mine: false,
            minutes_remaining: None,
        }
    }

    /// Up to two initials from the fantasy team name, for the logo fallback.
    pub fn monogram(&self) -> String {
        monogram(&self.name)
    }

    /// The points as `"1,247.50"`, or `None` pre-results.
    pub fn points_str(&self) -> Option<String> {
        self.points.map(scoring::format_points)
    }

    /// The whole minutes remaining, or `None` when the row has no live meters.
    pub fn minutes_str(&self) -> Option<String> {
        self.minutes_remaining.map(|m| m.round().to_string())
    }

    /// Band of the fill fraction: ≥50% `ok`, 10%–50% `warn`, <10% `low`.
    /// `None` minutes render as full (unreachable — the template guards on Some).
    pub fn minutes_band(&self) -> &'static str {
        let pct = self.minutes_pct_inner();
        if pct >= 50.0 {
            "ok"
        } else if pct >= 10.0 {
            "warn"
        } else {
            "low"
        }
    }

    /// Width of the fill bar, 0–100, rounded.
    pub fn minutes_pct(&self) -> u8 {
        self.minutes_pct_inner().round() as u8
    }

    fn minutes_pct_inner(&self) -> f64 {
        self.minutes_remaining
            .unwrap_or(LINEUP_REGULATION_MINUTES)
            .clamp(0.0, LINEUP_REGULATION_MINUTES)
            / LINEUP_REGULATION_MINUTES
            * 100.0
    }
}

/// Contest entrants with their scored points, for the rank table. `None`
/// stats leave every entrant un-scored so `rank_contest_entries` applies its
/// results gate.
pub fn entrants(entries: &[ContestEntry], stats: Option<&WeekStats>) -> Vec<ContestEntrant> {
    entries
        .iter()
        .map(|e| ContestEntrant {
            id: e.id,
            name: e.team_name.clone(),
            owner_name: e.owner_name.clone(),
            logo: e.logo.as_ref().map(|m| m.url()),
            points: stats.map(|s| score_lineup(&e.lineup, s)),
        })
        .collect()
}

/// Rank one contest's entrants. Results show only when every entrant scored
/// and some score beat zero — the gate the contest list applies to its winner
/// column. Otherwise rows keep case-insensitive name order with no rank,
/// points, or winner.
pub fn rank_contest_entries(entrants: Vec<ContestEntrant>) -> Vec<EntryRow> {
    let all_scored = !entrants.is_empty() && entrants.iter().all(|e| e.points.is_some());
    let top = all_scored
        .then(|| scoring::winning_score(entrants.iter().filter_map(|e| e.points)))
        .flatten();

    let mut rows: Vec<EntryRow> = entrants
        .into_iter()
        .map(|e| {
            let has_score = e.points.is_some();
            let points = top.and(e.points);
            let mut row = EntryRow::unscored(e);
            row.points = points;
            row.coverage = if has_score {
                Coverage::Complete
            } else {
                Coverage::Unavailable
            };
            row
        })
        .collect();

    let Some(top) = top else {
        rows.sort_by_key(|r| r.name.to_lowercase());
        return rows;
    };

    // `top` is Some only when every entrant scored, so every row has points.
    let points = |r: &EntryRow| r.points.expect("resolved: every entrant scored");
    rows.sort_by(|a, b| {
        standings::by_points_desc(&points(a), &points(b))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    let ranks = standings::shared_ranks(rows.iter().map(|r| r.points));
    for (r, rank) in rows.iter_mut().zip(ranks) {
        r.rank = rank;
        r.winner = scoring::ties(points(r), top);
    }
    rows
}

/// Rank contest entries against the process-local Tank01 snapshot.
///
/// Partial totals participate in numeric ordering but never crown a winner.
pub fn rank_live_entries(
    entries: &[ContestEntry],
    snapshot: &LiveContestSnapshot,
    now: OffsetDateTime,
) -> Vec<EntryRow> {
    let mut rows: Vec<EntryRow> = entries
        .iter()
        .map(|entry| {
            let total = live::score_live_lineup(&entry.lineup, snapshot, now);
            EntryRow {
                id: entry.id,
                rank: None,
                name: entry.team_name.clone(),
                owner_name: entry.owner_name.clone(),
                logo: entry.logo.as_ref().map(|logo| logo.url()),
                points: total.points,
                coverage: total.coverage,
                winner: false,
                mine: false,
                minutes_remaining: Some(live::lineup_minutes_remaining(
                    &entry.lineup,
                    snapshot,
                    now,
                )),
            }
        })
        .collect();
    rows.sort_by(|a, b| {
        match (a.points, b.points) {
            (Some(a), Some(b)) => standings::by_points_desc(&a, &b),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        }
        .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    let ranks = standings::shared_ranks(rows.iter().map(|row| row.points));
    for (row, rank) in rows.iter_mut().zip(ranks) {
        row.rank = rank;
    }
    rows
}

fn mark_my_entry(rows: &mut [EntryRow], mine: Option<EntryId>) -> Option<usize> {
    let mine = mine?;
    rows.iter_mut().enumerate().find_map(|(index, row)| {
        (row.id == mine).then(|| {
            row.mine = true;
            index
        })
    })
}

/// Entrants as rows in the order given (entry order), never scored: the
/// pre-kickoff roll call. The viewer's own entry, when given, is pinned first
/// and marked `mine`, so the page leads with "you're in".
fn rows_in_entry_order(entrants: Vec<ContestEntrant>, mine: Option<EntryId>) -> Vec<EntryRow> {
    let mut rows: Vec<EntryRow> = entrants.into_iter().map(EntryRow::unscored).collect();
    if let Some(i) = mark_my_entry(&mut rows, mine)
        && i > 0
    {
        let row = rows.remove(i);
        rows.insert(0, row);
    }
    rows
}

/// The slate's NFL week. `None` for an unpublished (empty) slate. Every contest
/// rule keeps a slate inside one week, so the first game names it.
pub fn slate_week(games: &[Game]) -> Option<Week> {
    games.first().map(|g| g.week)
}

/// A slate game for display: its id, matchup, and Eastern kickoff.
pub struct GameRow {
    pub id: String,
    pub matchup: String,
    pub kickoff: Option<String>,
}

/// A day for display: `"Sun, Oct 5"`. Formats in whatever offset `dt` carries,
/// so callers pass Eastern.
pub fn format_day(dt: OffsetDateTime) -> String {
    use time::macros::format_description;
    let fmt = format_description!("[weekday repr:short], [month repr:short] [day padding:none]");
    dt.format(fmt).unwrap_or_default()
}

/// A kickoff for display: `"Sun, Oct 5 · 1:00 PM"`. Formats in whatever offset
/// `et` carries, so callers pass Eastern.
pub fn format_kickoff(et: OffsetDateTime) -> String {
    use time::macros::format_description;
    let time_fmt = format_description!("[hour repr:12 padding:none]:[minute] [period]");
    format!(
        "{} · {}",
        format_day(et),
        et.format(time_fmt).unwrap_or_default()
    )
}

/// Display rows for a slate, chronological; games with no kickoff sort last.
pub fn game_rows(games: &[Game]) -> Vec<GameRow> {
    let mut rows: Vec<(Option<OffsetDateTime>, GameRow)> = games
        .iter()
        .map(|g| {
            let et = g.kickoff_eastern();
            (
                et,
                GameRow {
                    id: g.gsis_game_id.clone(),
                    matchup: format!("{} @ {}", g.away_team.0, g.home_team.0),
                    kickoff: et.map(format_kickoff),
                },
            )
        })
        .collect();
    // `is_none()` first puts kickoff-less games last; `Option` orders `None`
    // before `Some`.
    rows.sort_by(|a, b| {
        (a.0.is_none(), a.0, &a.1.matchup).cmp(&(b.0.is_none(), b.0, &b.1.matchup))
    });
    rows.into_iter().map(|(_, r)| r).collect()
}

/// A league team with no entry in an upcoming contest: the "still to enter"
/// roll call under the entered list.
pub struct WaitingRow {
    pub name: String,
    pub owner_name: String,
    pub logo: Option<String>,
    /// The viewer's own team, pinned first, highlighted, and linked to the
    /// lineup editor.
    pub mine: bool,
}

impl WaitingRow {
    /// Up to two initials from the fantasy team name, for the logo fallback.
    pub fn monogram(&self) -> String {
        monogram(&self.name)
    }
}

/// The league's teams with no entry in this contest, in the store's name
/// order. The viewer's own team, when it is one of them, is pinned first and
/// marked `mine`, so the page leads with "you're not in yet".
fn waiting_rows(
    teams: Vec<LeagueTeam>,
    entered: &HashSet<FantasyTeamId>,
    viewer: Option<FantasyTeamId>,
) -> Vec<WaitingRow> {
    let mut rows: Vec<WaitingRow> = teams
        .into_iter()
        .filter(|t| !entered.contains(&t.id))
        .map(|t| WaitingRow {
            name: t.name,
            owner_name: t.owner_name,
            logo: t.logo.as_ref().map(|m| m.url()),
            mine: Some(t.id) == viewer,
        })
        .collect();
    if let Some(i) = rows.iter().position(|r| r.mine)
        && i > 0
    {
        let row = rows.remove(i);
        rows.insert(0, row);
    }
    rows
}

/// The pre-kickoff header: entries stay open until the slate's first kickoff,
/// so the page leads with when that is and how long is left.
pub struct Upcoming {
    /// `"Sun, Nov 23 · 1:00 PM ET"`.
    pub kickoff: String,
    /// `"in 1 hour"` — how long until entries lock.
    pub countdown: String,
}

/// How far away `to` is, at the coarsest sensible unit: minutes under an hour,
/// hours under 36 hours, calendar days thereafter. Calendar-day counts use
/// Eastern dates. Kickoff is a deadline, so once it arrives the answer stops
/// counting rather than going negative.
pub fn time_until(from: OffsetDateTime, to: OffsetDateTime) -> String {
    let d = to - from;
    let minutes = d.whole_minutes();
    if minutes < 1 {
        return "now".to_string();
    }
    let (n, unit) = if minutes < 60 {
        (minutes, "minute")
    } else if d.whole_hours() < 36 {
        (d.whole_hours(), "hour")
    } else {
        let from_date = nfl_data::to_eastern(from).date();
        let to_date = nfl_data::to_eastern(to).date();
        (
            i64::from(to_date.to_julian_day() - from_date.to_julian_day()),
            "day",
        )
    };
    let s = if n == 1 { "" } else { "s" };
    format!("in {n} {unit}{s}")
}

/// Everything the contest details page shows.
pub struct ContestDetails {
    pub name: String,
    /// The slate, shown only before its first kickoff. Once the games are
    /// under way the entry rows carry the contest.
    pub games: Vec<GameRow>,
    pub rows: Vec<EntryRow>,
    /// League teams with no entry yet, empty outside the pre-kickoff window.
    pub waiting: Vec<WaitingRow>,
    /// The pre-kickoff header. `Some` exactly while the slate is upcoming —
    /// the same window in which `games` is non-empty.
    pub upcoming: Option<Upcoming>,
    /// The viewer's committed entry, when their team entered this contest.
    pub my_entry: Option<EntryId>,
    /// Whether lineups may still be entered: the slate is published and has
    /// not kicked off.
    pub entries_open: bool,
    pub score: ContestScoreMeta,
}

#[derive(Default)]
pub struct ContestScoreMeta {
    pub coverage: Coverage,
    pub events_url: Option<String>,
    pub active: bool,
    pub delayed: bool,
    pub official: bool,
}

fn contest_score_meta(
    source: &live::ContestScores,
    contest: ContestId,
    league_id: i64,
    rows: &[EntryRow],
    now: OffsetDateTime,
    stream_eligible: bool,
) -> ContestScoreMeta {
    match source {
        live::ContestScores::Official(_) => ContestScoreMeta {
            coverage: Coverage::Complete,
            events_url: None,
            active: false,
            delayed: false,
            official: true,
        },
        live::ContestScores::Provisional(snapshot) => {
            let delayed = snapshot.is_delayed(now);
            let has_partial = rows.iter().any(|row| row.coverage == Coverage::Partial);
            let has_unavailable = rows.iter().any(|row| row.coverage == Coverage::Unavailable);
            let has_usable = rows.iter().any(|row| row.coverage != Coverage::Unavailable);
            let coverage = if has_partial || (has_unavailable && has_usable) {
                Coverage::Partial
            } else if !rows.is_empty() && !has_usable {
                Coverage::Unavailable
            } else {
                Coverage::Complete
            };
            ContestScoreMeta {
                coverage,
                events_url: stream_eligible
                    .then(|| format!("/contests/{}/events?league_id={league_id}", contest.0)),
                active: snapshot.has_active_game(),
                delayed,
                official: false,
            }
        }
        live::ContestScores::Upcoming => ContestScoreMeta {
            coverage: Coverage::Unavailable,
            events_url: None,
            active: false,
            delayed: false,
            official: false,
        },
        live::ContestScores::Unavailable => ContestScoreMeta {
            coverage: Coverage::Unavailable,
            events_url: stream_eligible
                .then(|| format!("/contests/{}/events?league_id={league_id}", contest.0)),
            active: false,
            delayed: true,
            official: false,
        },
    }
}

pub async fn details_for_league(
    state: &AppState,
    league_id: i64,
    viewer: Option<FantasyTeamId>,
    id: ContestId,
) -> Result<Option<ContestDetails>, AppError> {
    let Some(contest) = store::by_id(state.db.reader(), id).await? else {
        return Ok(None);
    };
    let season = Season(state.config.season);
    let season_games = state.nfl.games(season).await?;
    let now = state.now_eastern();
    let games = published_slate(state.db.reader(), id, &season_games).await?;

    let entries = entries_store::by_contest(state.db.reader(), id, league_id).await?;

    let kickoffs: Vec<OffsetDateTime> = games.iter().filter_map(|g| g.kickoff_eastern()).collect();
    let first = kickoffs.iter().min().copied();
    let upcoming =
        contest_state(first, kickoffs.iter().max().copied(), now) == ContestState::Upcoming;

    let source = live::load_contest_scores(state, id).await?;
    let stream_eligible = live::stream_eligible(state, id, &source).await?;
    let my_entry = viewer.and_then(|team| {
        entries
            .iter()
            .find(|e| e.fantasy_team == team)
            .map(|e| e.id)
    });
    let mut rows = if upcoming {
        rows_in_entry_order(entrants(&entries, None), my_entry)
    } else {
        match &source {
            live::ContestScores::Official(stats) => {
                rank_contest_entries(entrants(&entries, Some(stats)))
            }
            live::ContestScores::Provisional(snapshot) => {
                rank_live_entries(&entries, snapshot, now)
            }
            live::ContestScores::Upcoming | live::ContestScores::Unavailable => {
                rank_contest_entries(entrants(&entries, None))
            }
        }
    };
    if !upcoming {
        let _ = mark_my_entry(&mut rows, my_entry);
    }

    let waiting = if upcoming {
        let entered: HashSet<FantasyTeamId> = entries.iter().map(|e| e.fantasy_team).collect();
        let teams = fantasy_teams_store::by_league(state.db.reader(), league_id).await?;
        waiting_rows(teams, &entered, viewer)
    } else {
        Vec::new()
    };

    let header = match (upcoming, first) {
        (true, Some(first)) => Some(Upcoming {
            kickoff: format!("{} ET", format_kickoff(first)),
            countdown: time_until(now, first),
        }),
        _ => None,
    };

    let score = contest_score_meta(&source, id, league_id, &rows, now, stream_eligible);
    Ok(Some(ContestDetails {
        name: contest.name,
        entries_open: upcoming && !games.is_empty(),
        games: if upcoming {
            game_rows(&games)
        } else {
            Vec::new()
        },
        rows,
        waiting,
        upcoming: header,
        my_entry,
        score,
    }))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use super::*;
    use crate::entries::Lineup;
    use crate::live::{LiveContestSnapshot, LiveGameState};
    use crate::tests::TestApp;
    use crate::tests::factories::{self, TeamOptions};
    use nfl_data::{
        Game, LiveGamePhase, PlayerWeekStats, Season, SeasonType, TeamAbbr as NflTeamAbbr,
        TeamWeekStats, Week,
    };
    use sqlx::SqlitePool;
    use time::OffsetDateTime;
    use time::macros::datetime;

    /// `id` is the entry the row must keep as `rank_contest_entries` reorders.
    fn entrant_with_id(id: i64, name: &str, points: Option<f64>) -> ContestEntrant {
        ContestEntrant {
            id: EntryId(id),
            name: name.to_string(),
            owner_name: "Owner".to_string(),
            logo: None,
            points,
        }
    }

    fn entrant(name: &str, points: Option<f64>) -> ContestEntrant {
        entrant_with_id(1, name, points)
    }

    fn game(id: &str, away: &str, home: &str, kickoff: Option<OffsetDateTime>) -> Game {
        Game {
            gsis_game_id: id.into(),
            season: Season(2025),
            week: Week(19),
            season_type: SeasonType::Post,
            kickoff,
            away_team: NflTeamAbbr(away.into()),
            home_team: NflTeamAbbr(home.into()),
            home_score: None,
            away_score: None,
        }
    }
    #[test]
    fn slate_contests_drop_unresolvable_games_but_keep_undated_known_games() {
        let schedule = SeasonSlates {
            games: vec![game("known", "BUF", "KC", None)],
            slates: std::collections::HashMap::from([
                (
                    ContestId(1),
                    vec![crate::contests::NflGameId("missing".into())],
                ),
                (
                    ContestId(2),
                    vec![crate::contests::NflGameId("known".into())],
                ),
            ]),
        };
        let rows = schedule.slate_contests(&[
            Contest {
                id: ContestId(1),
                name: "Missing".into(),
            },
            Contest {
                id: ContestId(2),
                name: "Undated".into(),
            },
        ]);

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, ContestId(2));
        assert!(rows[0].first.is_none());
    }

    #[test]
    fn time_until_picks_the_coarsest_sensible_unit() {
        let at = datetime!(2025 - 11 - 23 12:00 -5);
        let case = |later, expect: &str| assert_eq!(time_until(at, later), expect);
        case(datetime!(2025 - 11 - 23 12:00:30 -5), "now");
        case(datetime!(2025 - 11 - 23 11:00 -5), "now"); // past: stop counting
        case(datetime!(2025 - 11 - 23 12:01 -5), "in 1 minute");
        case(datetime!(2025 - 11 - 23 12:45 -5), "in 45 minutes");
        case(datetime!(2025 - 11 - 23 13:30 -5), "in 1 hour");
        case(datetime!(2025 - 11 - 24 12:00 -5), "in 24 hours");
        case(datetime!(2025 - 11 - 26 12:00 -5), "in 3 days");
        case(datetime!(2025 - 11 - 24 23:59 -5), "in 35 hours");
        case(datetime!(2025 - 11 - 25 00:00 -5), "in 2 days");
    }
    #[test]
    fn time_until_uses_eastern_dates_for_transition_instants() {
        assert_eq!(
            time_until(
                datetime!(2025 - 11 - 02 04:30 UTC),
                datetime!(2025 - 11 - 06 18:00 UTC),
            ),
            "in 4 days"
        );
        assert_eq!(
            time_until(
                datetime!(2025 - 11 - 01 23:30 -5),
                datetime!(2025 - 11 - 06 13:00 -5),
            ),
            "in 4 days"
        );
    }

    #[test]
    fn time_until_normalizes_utc_midnight_before_counting_days() {
        assert_eq!(
            time_until(
                datetime!(2025 - 11 - 23 12:00 UTC),
                datetime!(2025 - 11 - 25 00:00 UTC),
            ),
            "in 1 day"
        );
    }

    #[test]
    fn time_until_uses_calendar_days_for_multi_day_deadlines() {
        // Thursday afternoon to Sunday's kickoff spans three calendar dates,
        // although fewer than 72 hours elapse.
        let at = datetime!(2025 - 09 - 04 16:00 -4);
        let kickoff = datetime!(2025 - 09 - 07 13:00 -4);
        assert_eq!(time_until(at, kickoff), "in 3 days");
    }

    #[test]
    fn resolved_entrants_rank_desc_with_shared_ties_and_winners() {
        let rows = rank_contest_entries(vec![
            entrant("Ccc", Some(10.0)),
            entrant("Aaa", Some(50.0)),
            entrant("Bbb", Some(50.0)),
        ]);
        assert_eq!(rows[0].name, "Aaa");
        assert_eq!(rows[0].rank, Some(1));
        assert!(rows[0].winner);
        assert_eq!(rows[1].name, "Bbb");
        assert_eq!(rows[1].rank, Some(1), "ties share a rank");
        assert!(rows[1].winner, "ties all win");
        assert_eq!(rows[2].rank, Some(3));
        assert!(!rows[2].winner);
        assert_eq!(rows[2].points_str().as_deref(), Some("10.00"));
    }

    #[test]
    fn each_row_keeps_its_own_entry_id_through_the_sort() {
        // The rows come in name order and leave in score order, so a row that
        // kept its neighbour's id would link every reader to the wrong entry.
        let rows = rank_contest_entries(vec![
            entrant_with_id(10, "Ccc", Some(10.0)),
            entrant_with_id(50, "Aaa", Some(50.0)),
            entrant_with_id(30, "Bbb", Some(30.0)),
        ]);
        let pairs: Vec<(&str, EntryId)> = rows.iter().map(|r| (r.name.as_str(), r.id)).collect();
        assert_eq!(
            pairs,
            [
                ("Aaa", EntryId(50)),
                ("Bbb", EntryId(30)),
                ("Ccc", EntryId(10))
            ]
        );
    }

    #[test]
    fn co_winners_survive_a_sub_cent_scoring_difference() {
        // Two lineups whose sums land a bit apart display the same figure, so
        // both must take rank 1 and a trophy.
        let rows = rank_contest_entries(vec![
            entrant("Aaa", Some(50.0)),
            entrant("Bbb", Some(50.0 + 1e-12)),
        ]);
        assert_eq!(rows[0].points_str(), rows[1].points_str());
        assert_eq!(rows[0].rank, Some(1));
        assert_eq!(rows[1].rank, Some(1), "ties share a rank");
        assert!(rows[0].winner && rows[1].winner, "ties all win");
    }

    #[test]
    fn zero_top_score_keeps_the_pre_results_state() {
        // Week stats exist but the slate's games are unplayed: everyone
        // scores 0.0. The gate must hide points, ranks, and winners.
        let rows = rank_contest_entries(vec![
            entrant("Zebra", Some(0.0)),
            entrant("Apple", Some(0.0)),
        ]);
        assert_eq!(rows[0].name, "Apple", "name order pre-results");
        assert_eq!(rows[0].rank, None);
        assert_eq!(rows[0].points, None);
        assert!(!rows[0].winner);
        assert_eq!(rows[0].points_str(), None);
    }

    #[test]
    fn unresolved_week_keeps_name_order_without_points() {
        let rows = rank_contest_entries(vec![entrant("zebra", None), entrant("Apple", None)]);
        assert_eq!(rows[0].name, "Apple", "case-insensitive name order");
        assert_eq!(rows[1].name, "zebra");
        assert!(rows.iter().all(|r| r.rank.is_none() && r.points.is_none()));
    }
    #[test]
    fn minutes_view_methods_bands_and_label() {
        let row = |minutes| EntryRow {
            id: EntryId(1),
            rank: None,
            name: "Turf Titans".into(),
            owner_name: "Mike".into(),
            logo: None,
            points: None,
            coverage: Coverage::Unavailable,
            winner: false,
            mine: false,
            minutes_remaining: minutes,
        };

        let full = row(Some(342.0));
        assert_eq!(full.minutes_str().as_deref(), Some("342"));
        assert_eq!(full.minutes_band(), "ok");
        assert_eq!(full.minutes_pct(), 63);

        let ten = row(Some(54.0));
        assert_eq!(ten.minutes_band(), "warn", "exactly 10% is still warn");
        assert_eq!(ten.minutes_pct(), 10);

        let under = row(Some(53.9));
        assert_eq!(under.minutes_band(), "low", "just under 10% flips to low");
        assert_eq!(under.minutes_pct(), 10, "9.98 pct rounds back up to 10");

        let empty = row(None);
        assert_eq!(empty.minutes_str(), None);
        assert_eq!(empty.minutes_band(), "ok");
        assert_eq!(empty.minutes_pct(), 100);
    }

    #[test]
    fn provisional_rows_rank_numeric_totals_without_winners() {
        let game = game("live", "BUF", "KC", None);
        let player_a = nfl_data::Player {
            position: Some("RB".into()),
            ..factories::player("a", "A Runner")
        };
        let player_b = nfl_data::Player {
            position: Some("RB".into()),
            ..factories::player("b", "B Runner")
        };
        let identity = Arc::new(crate::live::identity::LiveIdentityIndex::from_sources(
            &[player_a, player_b],
            &[],
        ));
        let state = LiveGameState {
            game: game.clone(),
            provider_game_id: Some("live".into()),
            phase: LiveGamePhase::InProgress,
            period: Some("2".into()),
            clock: Some("08:00".into()),
            home_score: Some(14),
            away_score: Some(7),
            status_observed: true,
            players: HashMap::from([
                (
                    crate::entries::NflPlayerId("a".into()),
                    factories::rb_stats("a", 19, 100),
                ),
                (
                    crate::entries::NflPlayerId("b".into()),
                    factories::rb_stats("b", 19, 50),
                ),
            ]),
            player_section_present: true,
            player_identity_incomplete: false,
            player_observed_at: None,
            player_stale: false,
            defenses: HashMap::from([(
                NflTeamAbbr("KC".into()),
                factories::defense_stats("KC", "BUF", 19),
            )]),
            defense_observed_at: HashMap::new(),
            defense_stale: HashMap::new(),
            last_error: None,
        };
        let snapshot = LiveContestSnapshot {
            contest: ContestId(1),
            games: vec![game],
            stats: WeekStats::default(),
            game_states: HashMap::from([("live".into(), state)]),
            identity,
            identity_outcomes: Default::default(),
            latest_error: None,
            revision: 1,
        };
        let lineup = |id: &str| {
            let id = crate::entries::NflPlayerId(id.to_owned());
            Lineup {
                qb: id.clone(),
                rb1: id.clone(),
                rb2: id.clone(),
                wr1: id.clone(),
                wr2: id.clone(),
                wr3: id.clone(),
                te: id.clone(),
                flex: id,
                def: NflTeamAbbr("KC".into()),
            }
        };
        let entries = vec![
            ContestEntry {
                id: EntryId(1),
                fantasy_team: FantasyTeamId(1),
                team_name: "B".into(),
                owner_name: "Owner B".into(),
                logo: None,
                lineup: lineup("b"),
            },
            ContestEntry {
                id: EntryId(2),
                fantasy_team: FantasyTeamId(2),
                team_name: "A".into(),
                owner_name: "Owner A".into(),
                logo: None,
                lineup: lineup("a"),
            },
        ];

        let rows = rank_live_entries(&entries, &snapshot, datetime!(2025 - 09 - 07 18:00 UTC));

        assert_eq!(rows[0].id, EntryId(2));
        assert!(rows[0].points.unwrap() > rows[1].points.unwrap());
        assert!(rows.iter().all(|row| !row.winner));
        assert!(rows.iter().all(|row| row.rank.is_some()));
    }

    #[test]
    fn no_entrants_is_fine() {
        assert!(rank_contest_entries(vec![]).is_empty());
    }

    #[test]
    fn game_rows_are_chronological_with_eastern_kickoffs() {
        let rows = game_rows(&[
            game("b", "BUF", "JAX", Some(datetime!(2026-01-11 18:00 UTC))),
            game("a", "LA", "CAR", Some(datetime!(2026-01-10 21:30 UTC))),
            game("c", "SF", "PHI", None),
        ]);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].matchup, "LA @ CAR");
        assert_eq!(rows[0].kickoff.as_deref(), Some("Sat, Jan 10 · 4:30 PM"));
        assert_eq!(rows[1].matchup, "BUF @ JAX");
        assert_eq!(rows[1].kickoff.as_deref(), Some("Sun, Jan 11 · 1:00 PM"));
        assert_eq!(rows[2].matchup, "SF @ PHI", "kickoff-less games sort last");
        assert_eq!(rows[2].kickoff, None);
    }

    /// A contest whose one slate game kicks off 2025-09-07 17:00 UTC (1pm ET),
    /// seeded with one entry. Returns the contest id and the league id.
    async fn contest_with_kickoff(app: &TestApp, pool: &SqlitePool) -> (i64, i64) {
        let team = factories::team(pool, TeamOptions::default()).await;
        let wk1 = factories::contest(pool, "Week 1").await;
        sqlx::query(
            "INSERT INTO contest_games (contest_id, gsis_game_id) VALUES (?1, '2025_01_BUF_KC')",
        )
        .bind(wk1)
        .execute(pool)
        .await
        .unwrap();
        factories::full_entry(pool, wk1, team.team.id, "00-A").await;
        let mut g = factories::game("2025_01_BUF_KC", 1);
        g.kickoff = Some(datetime!(2025-09-07 17:00 UTC));
        app.nfl.seed_for_test(&[], &[g]).await.unwrap();
        (wk1, team.league.id)
    }

    #[sqlx::test]
    async fn details_lists_the_slate_before_kickoff(pool: SqlitePool) {
        let app = TestApp::from_pool_at(pool.clone(), datetime!(2025-09-06 12:00 UTC)).await;
        let (wk1, league) = contest_with_kickoff(&app, &pool).await;
        let details = details_for_league(&app.state(), league, None, ContestId(wk1))
            .await
            .expect("assemble")
            .expect("contest exists");
        assert_eq!(details.games.len(), 1, "slate shown before kickoff");
        assert_eq!(details.games[0].matchup, "BUF @ KC");
    }

    #[sqlx::test]
    async fn details_hides_the_slate_once_it_has_kicked_off(pool: SqlitePool) {
        // One minute past the only game's kickoff.
        let app = TestApp::from_pool_at(pool.clone(), datetime!(2025-09-07 17:01 UTC)).await;
        let (wk1, league) = contest_with_kickoff(&app, &pool).await;
        let details = details_for_league(&app.state(), league, None, ContestId(wk1))
            .await
            .expect("assemble")
            .expect("contest exists");
        assert!(details.games.is_empty(), "slate hidden once under way");
        assert_eq!(details.rows.len(), 1, "entries still listed");
    }

    #[sqlx::test]
    async fn details_ranks_a_resolved_contest(pool: SqlitePool) {
        // Two days past the slate's only kickoff: the contest has resolved.
        let app = TestApp::from_pool_at(pool.clone(), datetime!(2025-09-09 17:00 UTC)).await;
        let a = factories::team(
            &pool,
            TeamOptions {
                name: Some("Gridiron Giants".into()),
                ..Default::default()
            },
        )
        .await;
        let b = factories::team(
            &pool,
            TeamOptions {
                league: Some(a.league.clone()),
                name: Some("Turf Titans".into()),
                ..Default::default()
            },
        )
        .await;
        let wk1 = factories::contest(&pool, "Week 1").await;
        sqlx::query(
            "INSERT INTO contest_games (contest_id, gsis_game_id) VALUES (?1, '2025_01_BUF_KC')",
        )
        .bind(wk1)
        .execute(&pool)
        .await
        .unwrap();
        factories::full_entry(&pool, wk1, a.team.id, "00-A").await; // 100yd = 13.0
        factories::full_entry(&pool, wk1, b.team.id, "00-B").await; // 200yd = 23.0
        let mut g = factories::game("2025_01_BUF_KC", 1);
        g.kickoff = Some(datetime!(2025-09-07 17:00 UTC));
        app.nfl
            .seed_for_test(&[], std::slice::from_ref(&g))
            .await
            .unwrap();
        let buf_player = PlayerWeekStats {
            gsis_id: "00-BUF".into(),
            team: NflTeamAbbr("BUF".into()),
            opponent: Some(NflTeamAbbr("KC".into())),
            ..factories::player_stats("00-BUF", 1)
        };
        let buf_defense = TeamWeekStats {
            gsis_game_id: "2025_01_BUF_KC".into(),
            ..factories::scoreless_defense_stats("BUF", "KC", 1)
        };
        app.nfl
            .seed_week_stats_for_test(
                Season(2025),
                &[
                    PlayerWeekStats {
                        opponent: Some(NflTeamAbbr("BUF".into())),
                        ..factories::rb_stats("00-A", 1, 100)
                    },
                    PlayerWeekStats {
                        opponent: Some(NflTeamAbbr("BUF".into())),
                        ..factories::rb_stats("00-B", 1, 200)
                    },
                    buf_player,
                ],
                &[
                    factories::scoreless_defense_stats("KC", "BUF", 1),
                    buf_defense,
                ],
            )
            .await
            .unwrap();

        let details = details_for_league(&app.state(), a.league.id, None, ContestId(wk1))
            .await
            .expect("assemble")
            .expect("contest exists");
        assert_eq!(details.name, "Week 1");
        assert_eq!(details.rows.len(), 2);
        assert_eq!(details.rows[0].name, "Turf Titans");
        assert_eq!(details.rows[0].rank, Some(1));
        assert!(details.rows[0].winner);
        assert_eq!(details.rows[0].points_str().as_deref(), Some("23.00"));
        assert_eq!(details.rows[1].rank, Some(2));
        assert!(!details.rows[1].winner);
        assert!(details.games.is_empty(), "resolved slate hides its games");
        assert!(details.upcoming.is_none(), "no pre-kickoff header");
    }

    #[sqlx::test]
    async fn details_never_scores_an_upcoming_contest(pool: SqlitePool) {
        // Stats for the slate's week exist (e.g. replayed historical data),
        // but kickoff is still a day away: the page lists entrants without
        // points and leads with the pre-kickoff header instead.
        let app = TestApp::from_pool_at(pool.clone(), datetime!(2025-09-06 17:00 UTC)).await;
        let (wk1, league) = contest_with_kickoff(&app, &pool).await;
        app.nfl
            .seed_week_stats_for_test(
                Season(2025),
                &[factories::rb_stats("00-A", 1, 100)],
                &[factories::scoreless_defense_stats("KC", "BUF", 1)],
            )
            .await
            .unwrap();

        let details = details_for_league(&app.state(), league, None, ContestId(wk1))
            .await
            .expect("assemble")
            .expect("contest exists");
        assert_eq!(details.rows.len(), 1);
        assert_eq!(details.rows[0].points, None, "nothing played, no points");
        assert_eq!(details.rows[0].rank, None);
        assert_eq!(details.games.len(), 1, "slate still shown");
        let up = details.upcoming.expect("pre-kickoff header");
        assert_eq!(up.kickoff, "Sun, Sep 7 · 1:00 PM ET");
        assert_eq!(up.countdown, "in 24 hours");
        assert_eq!(
            details.waiting.len(),
            1,
            "the factory league's commissioner team has not entered"
        );
    }

    /// A league with an upcoming Week 1 slate and three teams; only `Zebra
    /// Herd` has entered. Returns (contest, league, non-entered team ids).
    async fn upcoming_with_one_entrant(
        app: &TestApp,
        pool: &SqlitePool,
    ) -> (i64, i64, FantasyTeamId, FantasyTeamId) {
        // A bare league, so the only teams are the three named here.
        let league = factories::bare_league(pool, Some("Sunday Funday".into())).await;
        let named = async |name: &str| {
            factories::team(
                pool,
                TeamOptions {
                    league: Some(league.clone()),
                    name: Some(name.to_string()),
                    ..Default::default()
                },
            )
            .await
        };
        let zebras = named("Zebra Herd").await;
        let aardvarks = named("aardvark alliance").await;
        let mustangs = named("Mustangs").await;
        let wk1 = factories::contest(pool, "Week 1").await;
        sqlx::query(
            "INSERT INTO contest_games (contest_id, gsis_game_id) VALUES (?1, '2025_01_BUF_KC')",
        )
        .bind(wk1)
        .execute(pool)
        .await
        .unwrap();
        factories::full_entry(pool, wk1, zebras.team.id, "00-A").await;
        let mut g = factories::game("2025_01_BUF_KC", 1);
        g.kickoff = Some(datetime!(2025-09-07 17:00 UTC));
        app.nfl.seed_for_test(&[], &[g]).await.unwrap();
        (
            wk1,
            league.id,
            FantasyTeamId(aardvarks.team.id),
            FantasyTeamId(mustangs.team.id),
        )
    }

    #[sqlx::test]
    async fn details_lists_teams_that_have_not_entered(pool: SqlitePool) {
        let app = TestApp::from_pool_at(pool.clone(), datetime!(2025-09-06 17:00 UTC)).await;
        let (wk1, league, _, _) = upcoming_with_one_entrant(&app, &pool).await;

        let details = details_for_league(&app.state(), league, None, ContestId(wk1))
            .await
            .expect("assemble")
            .expect("contest exists");
        let names: Vec<&str> = details.waiting.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(
            names,
            ["aardvark alliance", "Mustangs"],
            "entrants are excluded; the rest sort by name, case-insensitively"
        );
        assert!(details.waiting.iter().all(|r| !r.mine));
    }

    #[sqlx::test]
    async fn details_pins_and_marks_the_viewers_own_waiting_row(pool: SqlitePool) {
        let app = TestApp::from_pool_at(pool.clone(), datetime!(2025-09-06 17:00 UTC)).await;
        let (wk1, league, _, mustangs) = upcoming_with_one_entrant(&app, &pool).await;

        let details = details_for_league(&app.state(), league, Some(mustangs), ContestId(wk1))
            .await
            .expect("assemble")
            .expect("contest exists");
        let names: Vec<&str> = details.waiting.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(
            names,
            ["Mustangs", "aardvark alliance"],
            "the viewer's own team leads the list"
        );
        assert!(details.waiting[0].mine);
        assert!(!details.waiting[1].mine);
    }

    #[sqlx::test]
    async fn details_waiting_list_empties_once_every_team_enters(pool: SqlitePool) {
        let app = TestApp::from_pool_at(pool.clone(), datetime!(2025-09-06 17:00 UTC)).await;
        let (wk1, league, aardvarks, mustangs) = upcoming_with_one_entrant(&app, &pool).await;
        factories::full_entry(&pool, wk1, aardvarks.0, "00-B").await;
        factories::full_entry(&pool, wk1, mustangs.0, "00-C").await;

        let details = details_for_league(&app.state(), league, None, ContestId(wk1))
            .await
            .expect("assemble")
            .expect("contest exists");
        assert!(details.waiting.is_empty());
    }

    #[sqlx::test]
    async fn details_has_no_waiting_list_after_kickoff(pool: SqlitePool) {
        // Two days past kickoff: the roll call gives way to the scoreboard.
        let app = TestApp::from_pool_at(pool.clone(), datetime!(2025-09-09 17:00 UTC)).await;
        let (wk1, league, _, _) = upcoming_with_one_entrant(&app, &pool).await;

        let details = details_for_league(&app.state(), league, None, ContestId(wk1))
            .await
            .expect("assemble")
            .expect("contest exists");
        assert!(details.upcoming.is_none(), "not upcoming any more");
        assert!(details.waiting.is_empty());
    }

    #[sqlx::test]
    async fn details_lists_upcoming_entries_in_entry_order(pool: SqlitePool) {
        let app = TestApp::from_pool_at(pool.clone(), datetime!(2025-09-06 17:00 UTC)).await;
        let zebras = factories::team(
            &pool,
            TeamOptions {
                name: Some("Zebra Herd".into()),
                ..Default::default()
            },
        )
        .await;
        let aardvarks = factories::team(
            &pool,
            TeamOptions {
                league: Some(zebras.league.clone()),
                name: Some("Aardvark Alliance".into()),
                ..Default::default()
            },
        )
        .await;
        let wk1 = factories::contest(&pool, "Week 1").await;
        sqlx::query(
            "INSERT INTO contest_games (contest_id, gsis_game_id) VALUES (?1, '2025_01_BUF_KC')",
        )
        .bind(wk1)
        .execute(&pool)
        .await
        .unwrap();
        // Zebras enter first, Aardvarks second: the roll call keeps that
        // order rather than sorting by name.
        factories::full_entry(&pool, wk1, zebras.team.id, "00-A").await;
        factories::full_entry(&pool, wk1, aardvarks.team.id, "00-B").await;
        let mut g = factories::game("2025_01_BUF_KC", 1);
        g.kickoff = Some(datetime!(2025-09-07 17:00 UTC));
        app.nfl.seed_for_test(&[], &[g]).await.unwrap();

        let details = details_for_league(&app.state(), zebras.league.id, None, ContestId(wk1))
            .await
            .expect("assemble")
            .expect("contest exists");
        assert!(details.upcoming.is_some());
        assert!(details.entries_open, "published, pre-kickoff slate is open");
        assert!(details.my_entry.is_none(), "no viewer, no own entry");
        let names: Vec<&str> = details.rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["Zebra Herd", "Aardvark Alliance"]);

        // A viewer who entered gets their row pinned first and marked.
        let details = details_for_league(
            &app.state(),
            zebras.league.id,
            Some(FantasyTeamId(aardvarks.team.id)),
            ContestId(wk1),
        )
        .await
        .expect("assemble")
        .expect("contest exists");
        assert!(details.my_entry.is_some());
        assert_eq!(details.rows[0].name, "Aardvark Alliance");
        assert!(details.rows[0].mine);
        assert!(!details.rows[1].mine);
    }

    #[sqlx::test]
    async fn details_none_for_unknown_contest(pool: SqlitePool) {
        let app = TestApp::from_pool(pool.clone()).await;
        let fixture = factories::team(&pool, TeamOptions::default()).await;
        let details = details_for_league(&app.state(), fixture.league.id, None, ContestId(999))
            .await
            .expect("assemble");
        assert!(details.is_none());
    }

    #[sqlx::test]
    async fn details_excludes_other_leagues(pool: SqlitePool) {
        let app = TestApp::from_pool(pool.clone()).await;
        let mine = factories::team(&pool, TeamOptions::default()).await;
        let other = factories::team(&pool, TeamOptions::default()).await;
        let wk1 = factories::contest(&pool, "Week 1").await;
        factories::full_entry(&pool, wk1, other.team.id, "00-B").await;

        let details = details_for_league(&app.state(), mine.league.id, None, ContestId(wk1))
            .await
            .expect("assemble")
            .expect("contest exists");
        assert!(details.rows.is_empty(), "other league's entries invisible");
    }

    #[sqlx::test]
    async fn details_hides_points_for_an_unfrozen_slate(pool: SqlitePool) {
        // No contest_games rows: the slate is still live, so no result is
        // final and the page must stay in its pre-results state even though
        // the week's stats exist.
        let app = TestApp::from_pool(pool.clone()).await;
        let fixture = factories::team(&pool, TeamOptions::default()).await;
        let wk = factories::contest(&pool, "Week 1").await;
        factories::full_entry(&pool, wk, fixture.team.id, "00-A").await;
        app.nfl
            .seed_for_test(&[], &[factories::game("2025_01_BUF_KC", 1)])
            .await
            .unwrap();
        app.nfl
            .seed_week_stats_for_test(
                Season(2025),
                &[factories::rb_stats("00-A", 1, 100)],
                &[factories::scoreless_defense_stats("KC", "BUF", 1)],
            )
            .await
            .unwrap();

        let details = details_for_league(&app.state(), fixture.league.id, None, ContestId(wk))
            .await
            .expect("assemble")
            .expect("contest exists");
        assert_eq!(details.rows.len(), 1);
        assert_eq!(details.rows[0].points, None, "live slate scores nothing");
        assert_eq!(details.rows[0].rank, None);
    }

    #[sqlx::test]
    async fn details_without_resolvable_week_hides_points(pool: SqlitePool) {
        let app = TestApp::from_pool(pool.clone()).await;
        let fixture = factories::team(&pool, TeamOptions::default()).await;
        let wk = factories::contest(&pool, "Week 1").await;
        // Materialized game absent from the seeded schedule -> week unresolvable.
        sqlx::query(
            "INSERT INTO contest_games (contest_id, gsis_game_id) VALUES (?1, '2025_99_BUF_KC')",
        )
        .bind(wk)
        .execute(&pool)
        .await
        .unwrap();
        factories::full_entry(&pool, wk, fixture.team.id, "00-A").await;

        let details = details_for_league(&app.state(), fixture.league.id, None, ContestId(wk))
            .await
            .expect("assemble")
            .expect("contest exists");
        assert_eq!(details.rows.len(), 1);
        assert_eq!(details.rows[0].points, None);
        assert_eq!(details.rows[0].rank, None);
        assert!(
            details.games.is_empty(),
            "schedule-less frozen games render no game rows"
        );
    }
}

#[cfg(test)]
mod week_tests {
    use super::*;
    use time::macros::date;

    #[test]
    fn nfl_week_opens_on_monday() {
        // Sunday slate: the Monday six days earlier.
        assert_eq!(
            nfl_week_monday(date!(2025 - 09 - 07)),
            date!(2025 - 09 - 01)
        );
        // Thursday opener: the Monday three days earlier.
        assert_eq!(
            nfl_week_monday(date!(2025 - 11 - 27)),
            date!(2025 - 11 - 24)
        );
        // A Saturday opener (playoff weekend): the Monday five days earlier.
        assert_eq!(
            nfl_week_monday(date!(2026 - 01 - 10)),
            date!(2026 - 01 - 05)
        );
        // A Monday game is the prior week's finale: the Monday before.
        assert_eq!(
            nfl_week_monday(date!(2025 - 09 - 08)),
            date!(2025 - 09 - 01)
        );
    }
}
