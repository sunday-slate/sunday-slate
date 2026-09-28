//! Pure view assembly for the team detail page: a fantasy team's scored
//! contests ordered best-first, top-10 flags, and the score to beat.

use crate::contests::{Contest, ContestId};
use crate::scoring::format_points;
use crate::standings::standings::{COUNTING_SCORES, by_points_desc, top_n_sum_sorted};

/// One scored contest on the team detail page.
pub struct ContestRow {
    /// The `/contests/{id}` link target, and the tie-break in `season_view`.
    pub contest: ContestId,
    pub name: String,
    pub points: f64,
    /// In the top 10 — counts toward the season total.
    pub counts: bool,
    /// Lowest of the top 10 — the score to beat.
    pub score_to_beat: bool,
}

impl ContestRow {
    /// The points as `"1,247.50"`.
    pub fn points_str(&self) -> String {
        format_points(self.points)
    }
}

/// A fantasy team's season: rows by score descending, total from the
/// counting rows, `None` when no contest scored.
pub struct SeasonView {
    pub rows: Vec<ContestRow>,
    pub total: Option<f64>,
}

impl SeasonView {
    /// The total as `"1,247.50"`, or `None` when no contest scored.
    pub fn total_str(&self) -> Option<String> {
        self.total.map(format_points)
    }

    /// The header label under the total: "season total" at 10 or fewer scored
    /// contests, "best-10 total" above that.
    pub fn total_label(&self) -> &'static str {
        if self.rows.len() > COUNTING_SCORES {
            "best-10 total"
        } else {
            "season total"
        }
    }
}

/// Order scored contests best-first, ties by contest id, then flag the
/// counting rows. Only a team with ten or more scores gets a score to beat.
pub fn season_view(scored: Vec<(Contest, f64)>) -> SeasonView {
    let mut scored = scored;
    scored.sort_by(|a, b| by_points_desc(&a.1, &b.1).then_with(|| a.0.id.0.cmp(&b.0.id.0)));
    let total = (!scored.is_empty()).then(|| top_n_sum_sorted(scored.iter().map(|(_, p)| *p)));
    let displaceable = scored.len() >= COUNTING_SCORES;
    let rows = scored
        .into_iter()
        .enumerate()
        .map(|(i, (contest, points))| ContestRow {
            contest: contest.id,
            name: contest.name,
            points,
            counts: i < COUNTING_SCORES,
            score_to_beat: displaceable && i == COUNTING_SCORES - 1,
        })
        .collect();
    SeasonView { rows, total }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contest(id: i64, name: &str) -> Contest {
        Contest {
            id: ContestId(id),
            name: name.to_string(),
        }
    }

    /// Contests 1..=n, contest i scoring i points.
    fn scored(n: i64) -> Vec<(Contest, f64)> {
        (1..=n)
            .map(|i| (contest(i, &format!("Week {i}")), i as f64))
            .collect()
    }

    #[test]
    fn orders_by_score_descending_then_contest_id() {
        let view = season_view(vec![
            (contest(1, "Week 1"), 10.0),
            (contest(2, "Week 2"), 30.0),
            (contest(3, "Week 3"), 10.0),
        ]);
        let order: Vec<i64> = view.rows.iter().map(|r| r.contest.0).collect();
        assert_eq!(order, vec![2, 1, 3], "score desc, tie broken by id");
    }

    #[test]
    fn under_ten_scores_all_count_and_nothing_to_beat() {
        let view = season_view(scored(3));
        assert!(view.rows.iter().all(|r| r.counts));
        assert!(view.rows.iter().all(|r| !r.score_to_beat));
        assert_eq!(view.total, Some(6.0));
    }

    #[test]
    fn exactly_ten_scores_marks_the_last() {
        let view = season_view(scored(10));
        assert!(view.rows.iter().all(|r| r.counts));
        let marked: Vec<i64> = view
            .rows
            .iter()
            .filter(|r| r.score_to_beat)
            .map(|r| r.contest.0)
            .collect();
        assert_eq!(marked, vec![1], "lowest of the ten");
        assert_eq!(view.total, Some(55.0));
    }

    #[test]
    fn over_ten_scores_mutes_the_tail() {
        let view = season_view(scored(12));
        assert_eq!(view.rows.len(), 12);
        assert!(view.rows[..10].iter().all(|r| r.counts));
        assert!(view.rows[10..].iter().all(|r| !r.counts));
        assert!(view.rows[9].score_to_beat, "tenth row is the score to beat");
        assert_eq!(
            view.rows.iter().filter(|r| r.score_to_beat).count(),
            1,
            "exactly one mark"
        );
        // 12+11+...+3 = 75; the 2.0 and 1.0 rows do not count.
        assert_eq!(view.total, Some(75.0));
    }

    #[test]
    fn total_label_stays_season_total_through_ten_scores() {
        assert_eq!(season_view(scored(10)).total_label(), "season total");
    }

    #[test]
    fn total_label_flips_to_best_ten_at_eleven_scores() {
        assert_eq!(season_view(scored(11)).total_label(), "best-10 total");
    }

    #[test]
    fn no_scores_means_no_rows_and_blank_total() {
        let view = season_view(vec![]);
        assert!(view.rows.is_empty());
        assert_eq!(view.total, None);
        assert_eq!(view.total_str(), None);
    }

    #[test]
    fn standings_and_team_detail_totals_agree_on_the_same_scores() {
        // More than ten scores, so the truncation bites. Both paths must
        // total the same raw scores identically.
        use crate::fantasy_teams::FantasyTeamId;
        use crate::standings::standings::{ScoredFantasyTeam, rank};

        let raw: Vec<(i64, f64)> = (1..=13).map(|c| (c, c as f64)).collect();

        let standings_total = rank(vec![ScoredFantasyTeam {
            id: FantasyTeamId(1),
            name: "Alpha".into(),
            owner_name: "Owner".into(),
            logo: None,
            entered: raw.iter().map(|(c, _)| ContestId(*c)).collect(),
            scores: raw.iter().map(|(c, p)| (ContestId(*c), *p)).collect(),
        }])[0]
            .total;

        let team_detail_total = season_view(
            raw.iter()
                .map(|(c, p)| (contest(*c, &format!("Week {c}")), *p))
                .collect(),
        )
        .total;

        assert_eq!(standings_total, team_detail_total);
        assert_eq!(standings_total, Some(85.0), "top 10 of 1..=13 = 4+..+13");
    }
}
