//! Pure standings aggregation: fantasy teams ranked by sum-of-top-10, with
//! contest wins. No DB access — each team arrives owning its contest scores.

use std::cmp::Ordering;
use std::collections::HashMap;

use itertools::Itertools;

use crate::contests::ContestId;
use crate::fantasy_teams::{FantasyTeamId, monogram};
use crate::scoring::{format_points, ties, winning_score};

/// A fantasy team with its entered contests and its season's computed
/// contest scores.
pub struct ScoredFantasyTeam {
    pub id: FantasyTeamId,
    pub name: String,
    pub owner_name: String,
    pub logo: Option<String>,
    pub entered: Vec<ContestId>,
    pub scores: Vec<(ContestId, f64)>,
}

/// A rendered standings row. `fantasy_team` is the `/teams/{id}` link target.
pub struct StandingRow {
    pub fantasy_team: FantasyTeamId,
    pub rank: Option<u32>,
    pub name: String,
    pub owner_name: String,
    pub logo: Option<String>,
    pub total: Option<f64>,
    pub wins: u32,
}

/// How many of a team's contest scores count toward its season total. Both
/// the standings and the team detail page read it from here.
pub const COUNTING_SCORES: usize = 10;

/// Order two scores best-first. Compose a tie-break onto it with `then_with`.
/// `total_cmp` orders NaN instead of panicking on it.
pub fn by_points_desc(a: &f64, b: &f64) -> Ordering {
    b.total_cmp(a)
}

/// Shared-tie ranks (1, 1, 3) for scores already ordered best-first, compared
/// at display precision so two rows showing the same figure share a rank.
/// `None` scores rank `None`; they sort last, so they break no rank run.
pub fn shared_ranks(scores: impl IntoIterator<Item = Option<f64>>) -> Vec<Option<u32>> {
    let mut prev: Option<f64> = None;
    let mut rank = 0u32;
    scores
        .into_iter()
        .enumerate()
        .map(|(i, score)| {
            let score = score?;
            if !matches!(prev, Some(p) if ties(score, p)) {
                rank = (i + 1) as u32;
                prev = Some(score);
            }
            Some(rank)
        })
        .collect()
}

/// Sum of the first `COUNTING_SCORES` values of a sequence already ordered
/// by `by_points_desc`.
pub fn top_n_sum_sorted(points: impl IntoIterator<Item = f64>) -> f64 {
    points.into_iter().take(COUNTING_SCORES).sum()
}

/// Sum of the `COUNTING_SCORES` highest values in an unordered `points`.
pub fn top_n_sum(points: &[f64]) -> f64 {
    let mut sorted = points.to_vec();
    sorted.sort_by(by_points_desc);
    top_n_sum_sorted(sorted)
}

impl StandingRow {
    /// Up to two initials from the fantasy team name, for the logo fallback.
    pub fn monogram(&self) -> String {
        monogram(&self.name)
    }

    /// The total as `"1,247.50"`, or `None` when the team has no scored
    /// contest.
    pub fn total_str(&self) -> Option<String> {
        self.total.map(format_points)
    }
}

/// Rank fantasy teams by sum-of-top-10; tally contest wins; shared-tie ranks;
/// teams with no scored contest last by name with a blank total.
pub fn rank(teams: Vec<ScoredFantasyTeam>) -> Vec<StandingRow> {
    // Contest wins: per contest, every fantasy team at the max score gets +1.
    let by_contest: HashMap<ContestId, Vec<(FantasyTeamId, f64)>> = teams
        .iter()
        .flat_map(|t| {
            t.scores
                .iter()
                .map(|(contest, points)| (*contest, (t.id, *points)))
        })
        .into_group_map();
    let mut wins: HashMap<FantasyTeamId, u32> = HashMap::new();
    for entries in by_contest.values() {
        let Some(max) = winning_score(entries.iter().map(|(_, p)| *p)) else {
            continue;
        };
        for (team, p) in entries {
            if ties(*p, max) {
                *wins.entry(*team).or_default() += 1;
            }
        }
    }

    // Rows with totals (top-10 sum) or None for unscored teams.
    let mut rows: Vec<StandingRow> = teams
        .into_iter()
        .map(|t| {
            let total = (!t.scores.is_empty()).then(|| {
                let pts: Vec<f64> = t.scores.iter().map(|(_, p)| *p).collect();
                top_n_sum(&pts)
            });
            StandingRow {
                fantasy_team: t.id,
                rank: None,
                name: t.name,
                owner_name: t.owner_name,
                logo: t.logo,
                total,
                wins: wins.get(&t.id).copied().unwrap_or(0),
            }
        })
        .collect();

    // Sort: scored (by total desc) before unscored; ties/unscored by name asc.
    rows.sort_by(|a, b| match (a.total, b.total) {
        (Some(x), Some(y)) => {
            by_points_desc(&x, &y).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        }
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
    });

    let ranks = shared_ranks(rows.iter().map(|r| r.total));
    for (row, rank) in rows.iter_mut().zip(ranks) {
        row.rank = rank;
    }

    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn team(id: i64, name: &str, scores: &[(i64, f64)]) -> ScoredFantasyTeam {
        ScoredFantasyTeam {
            id: FantasyTeamId(id),
            name: name.to_string(),
            owner_name: "Owner".into(),
            logo: None,
            entered: scores.iter().map(|(c, _)| ContestId(*c)).collect(),
            scores: scores.iter().map(|(c, p)| (ContestId(*c), *p)).collect(),
        }
    }

    #[test]
    fn ranks_by_total_desc_and_counts_only_top_ten() {
        // Team 1 enters 11 contests scoring 1..=11; only the best 10 (2..=11 = 65) count.
        let one: Vec<(i64, f64)> = (1..=11).map(|c| (c, c as f64)).collect();
        let rows = rank(vec![
            team(1, "Alpha", &one),
            team(2, "Bravo", &[(1, 100.0)]),
        ]);
        assert_eq!(rows[0].name, "Bravo");
        assert_eq!(rows[0].rank, Some(1));
        assert_eq!(rows[0].total, Some(100.0));
        assert_eq!(
            rows[0].fantasy_team,
            FantasyTeamId(2),
            "route token rides through"
        );
        assert_eq!(rows[1].name, "Alpha");
        assert_eq!(rows[1].total, Some(65.0), "top 10 of 1..=11 = 2+..+11");
    }

    #[test]
    fn ties_share_a_rank() {
        // Two teams tie at 50 in contest 1 (rank 1,1, a win each); third is rank 3.
        let rows = rank(vec![
            team(1, "Aaa", &[(1, 50.0)]),
            team(2, "Bbb", &[(1, 50.0)]),
            team(3, "Ccc", &[(1, 10.0)]),
        ]);
        assert_eq!(rows[0].rank, Some(1));
        assert_eq!(rows[1].rank, Some(1));
        assert_eq!(rows[2].rank, Some(3));
        assert_eq!(rows[0].wins, 1);
        assert_eq!(rows[1].wins, 1);
        assert_eq!(rows[2].wins, 0);
    }

    #[test]
    fn totals_that_display_alike_share_a_rank() {
        // Summation order can leave two identical seasons differing in the
        // last bit. The page shows one figure, so it must show one rank.
        let rows = rank(vec![
            team(1, "Aaa", &[(1, 50.0)]),
            team(2, "Bbb", &[(1, 50.0 + 1e-12)]),
        ]);
        assert_eq!(rows[0].total_str(), rows[1].total_str());
        assert_eq!(rows[0].rank, Some(1));
        assert_eq!(rows[1].rank, Some(1));
    }

    #[test]
    fn a_contest_nobody_scored_in_credits_no_win() {
        // Entries are in, the week resolves, but the slate has not been played:
        // every lineup sits at 0.0. Crediting a win to all of them would
        // contradict the contest list, which shows no winner for that contest.
        let rows = rank(vec![
            team(1, "Aaa", &[(1, 0.0)]),
            team(2, "Bbb", &[(1, 0.0)]),
        ]);
        assert_eq!(rows[0].wins, 0);
        assert_eq!(rows[1].wins, 0);
    }

    #[test]
    fn wins_survive_a_sub_cent_scoring_difference() {
        // Two identical lineups whose sums land a bit apart must both win.
        let rows = rank(vec![
            team(1, "Aaa", &[(1, 50.0)]),
            team(2, "Bbb", &[(1, 50.0 + 1e-12)]),
        ]);
        assert_eq!(rows[0].wins, 1);
        assert_eq!(rows[1].wins, 1);
    }

    #[test]
    fn unscored_teams_sort_last_by_name_with_blank_total() {
        let rows = rank(vec![
            team(1, "Zebra", &[]),
            team(2, "Scored", &[(1, 10.0)]),
            team(3, "Apple", &[]),
        ]);
        assert_eq!(rows[0].name, "Scored");
        assert_eq!(rows[0].total, Some(10.0));
        assert_eq!(rows[1].name, "Apple");
        assert_eq!(rows[1].total, None);
        assert_eq!(rows[1].rank, None);
        assert_eq!(rows[2].name, "Zebra");
    }

    #[test]
    fn monogram_and_total_str_formatting() {
        let mut r = StandingRow {
            fantasy_team: FantasyTeamId(1),
            rank: Some(1),
            name: "Gridiron Giants".into(),
            owner_name: "Mike".into(),
            logo: None,
            total: Some(1247.5),
            wins: 3,
        };
        assert_eq!(r.monogram(), "GG");
        assert_eq!(r.total_str().as_deref(), Some("1,247.50"));
        r.total = None;
        assert_eq!(r.total_str(), None);
    }

    #[test]
    fn total_str_rounds_up_into_whole_part() {
        let mut r = StandingRow {
            fantasy_team: FantasyTeamId(1),
            rank: Some(1),
            name: "Carry".into(),
            owner_name: "Owner".into(),
            logo: None,
            total: Some(9.999),
            wins: 0,
        };
        // Must round up to "10.00", not truncate-then-round to "9.100"/"9.1000".
        assert_eq!(r.total_str().as_deref(), Some("10.00"));
        assert_ne!(r.total_str().as_deref(), Some("9.100"));
        assert_ne!(r.total_str().as_deref(), Some("9.1000"));

        r.total = Some(999.999);
        assert_eq!(r.total_str().as_deref(), Some("1,000.00"));
    }

    #[test]
    fn total_str_keeps_negative_sign() {
        let r = StandingRow {
            fantasy_team: FantasyTeamId(1),
            rank: None,
            name: "Negative".into(),
            owner_name: "Owner".into(),
            logo: None,
            total: Some(-50.25),
            wins: 0,
        };
        assert_eq!(r.total_str().as_deref(), Some("-50.25"));
    }
}
