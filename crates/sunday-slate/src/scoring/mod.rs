pub mod service;

use crate::entries::{Lineup, NflPlayerId};
use nfl_data::{
    PlayerWeekStats as NflPlayerWeekStats, TeamAbbr as NflTeamAbbr,
    TeamWeekStats as NflTeamWeekStats,
};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq)]
pub struct ScoreLine {
    pub stat: &'static str,
    /// `None` for a threshold bonus, whose name already states the quantity.
    pub quantity: Option<f64>,
    pub points: f64,
}

impl ScoreLine {
    /// `"2 RuTD"`, or just the stat name for a threshold bonus (`"100+ RuY Gm"`).
    pub fn label(&self) -> String {
        match self.quantity {
            Some(q) => format!("{} {}", q as i64, self.stat),
            None => self.stat.to_string(),
        }
    }
    /// This line's points as `"12.00"`.
    pub fn points_str(&self) -> String {
        format_points(self.points)
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Score {
    pub total: f64,
    pub lines: Vec<ScoreLine>,
}

impl Score {
    /// `"247 PaY, 2 PaTD, 300+ PaY Gm"` — one clause per scoring line, in
    /// scoring order.
    pub fn stat_line(&self) -> String {
        self.lines
            .iter()
            .map(ScoreLine::label)
            .collect::<Vec<_>>()
            .join(", ")
    }
}

fn line(stat: &'static str, quantity: f64, points: f64) -> ScoreLine {
    ScoreLine {
        stat,
        quantity: Some(quantity),
        points,
    }
}

fn threshold(stat: &'static str, points: f64) -> ScoreLine {
    ScoreLine {
        stat,
        quantity: None,
        points,
    }
}

/// FanDuel points for one player's week (offense). Rules transcribed from
/// fanduel-scraper/RECON.md.
pub fn score_player(s: &NflPlayerWeekStats) -> Score {
    let mut lines = Vec::new();
    let mut push = |stat, qty: f64, per: f64| {
        if qty != 0.0 {
            lines.push(line(stat, qty, qty * per));
        }
    };
    push("PaY", s.passing_yards as f64, 0.04);
    push("PaTD", s.passing_tds as f64, 4.0);
    push("I", s.passing_interceptions as f64, -1.0);
    push("RuY", s.rushing_yards as f64, 0.1);
    push("RuTD", s.rushing_tds as f64, 6.0);
    push("Re", s.receptions as f64, 0.5);
    push("ReY", s.receiving_yards as f64, 0.1);
    push("ReTD", s.receiving_tds as f64, 6.0);
    push("FU/L", s.fumbles_lost as f64, -2.0);
    push("2PC", s.two_point_conversions as f64, 2.0);
    push("KR/PR TD", s.special_teams_tds as f64, 6.0);
    push("FU/TD", s.fumble_recovery_tds as f64, 6.0);
    if s.passing_yards >= 300 {
        lines.push(threshold("300+ PaY Gm", 3.0));
    }
    if s.rushing_yards >= 100 {
        lines.push(threshold("100+ RuY Gm", 3.0));
    }
    if s.receiving_yards >= 100 {
        lines.push(threshold("100+ ReY Gm", 3.0));
    }
    let total = lines.iter().map(|l| l.points).sum();
    Score { total, lines }
}

/// FanDuel points-allowed tier from the defense's points-allowed count.
/// Uses nfl-data's points_allowed (excludes non-defensive scoring).
fn points_allowed_points(pa: i32) -> f64 {
    match pa {
        0 => 10.0,
        1..=6 => 7.0,
        7..=13 => 4.0,
        14..=20 => 1.0,
        21..=27 => 0.0,
        28..=34 => -1.0,
        _ => -4.0,
    }
}

/// FanDuel points for one team defense's week. All inputs are pbp-derived, so
/// every FanDuel D/ST category is covered.
pub fn score_defense(t: &NflTeamWeekStats) -> Score {
    let mut lines = Vec::new();
    let mut push = |stat, qty: f64, per: f64| {
        if qty != 0.0 {
            lines.push(line(stat, qty, qty * per));
        }
    };
    push("S", t.sacks as f64, 1.0);
    push("I", t.interceptions as f64, 2.0);
    push("FR", t.fumble_recoveries as f64, 2.0);
    push("TD", t.touchdowns as f64, 6.0);
    push("DE/SF", t.safeties as f64, 2.0);
    push("DE/B", t.blocked_kicks as f64, 2.0);
    push("DE/XPR", t.conversion_returns as f64, 2.0);
    let pts = points_allowed_points(t.points_allowed);
    if pts != 0.0 {
        lines.push(line("DE/PA", t.points_allowed as f64, pts));
    }
    let total = lines.iter().map(|l| l.points).sum();
    Score { total, lines }
}

/// One NFL week's stat lookups, typed. A `WeekStats` is week-scoped, so
/// scoring needs no week parameter. The default is empty: nothing scores.
#[derive(Default, Clone, Debug)]
pub struct WeekStats {
    pub players: HashMap<NflPlayerId, NflPlayerWeekStats>,
    pub defenses: HashMap<NflTeamAbbr, NflTeamWeekStats>,
}

/// Total FanDuel points for a lineup against one week's stats. A slot whose
/// stat line is absent (bye/inactive) contributes 0 — the only zero-case
/// left, since a `Lineup` is total.
pub fn score_lineup(lineup: &Lineup, stats: &WeekStats) -> f64 {
    let player_points: f64 = lineup
        .player_ids()
        .iter()
        .filter_map(|id| stats.players.get(id))
        .map(|p| score_player(p).total)
        .sum();
    let defense_points = stats
        .defenses
        .get(&lineup.def)
        .map(|t| score_defense(t).total)
        .unwrap_or(0.0);
    player_points + defense_points
}

/// The score that wins a contest: the highest, and only when it beats zero.
/// A slate nobody has played yet scores every lineup at 0.0, and that has no
/// winner.
pub fn winning_score(points: impl IntoIterator<Item = f64>) -> Option<f64> {
    points.into_iter().reduce(f64::max).filter(|top| *top > 0.0)
}

/// Whether two scores count as the same score.
///
/// Rounds the way `format_points` does, so a co-winner is credited exactly
/// when the page shows equal figures. Exact `f64` equality would drop a
/// rightful co-winner whenever summation order left two identical stat lines
/// differing in the last bit.
pub fn ties(a: f64, b: f64) -> bool {
    hundredths(a) == hundredths(b)
}

/// A score in whole hundredths — the precision at which the league reads a
/// score, and so the precision at which two scores are the same score.
fn hundredths(points: f64) -> i64 {
    (points * 100.0).round() as i64
}

/// A score as the reader sees it: `1247.5` -> `"1,247.50"`.
///
/// Rounds to whole hundredths first, rather than `trunc`/`fract`, so `9.999`
/// carries into the whole part instead of giving `"9.100"`, and a negative
/// score keeps its sign.
pub fn format_points(points: f64) -> String {
    let hundredths = hundredths(points);
    let sign = if hundredths < 0 { "-" } else { "" };
    let hundredths = hundredths.unsigned_abs();
    let whole = group_digits(&(hundredths / 100).to_string());
    let frac = hundredths % 100;
    format!("{sign}{whole}.{frac:02}")
}

/// A whole-dollar figure as the reader sees it: `5700` -> `"5,700"`.
pub fn format_thousands(v: i64) -> String {
    let sign = if v < 0 { "-" } else { "" };
    format!("{sign}{}", group_digits(&v.unsigned_abs().to_string()))
}

/// `"1234567"` -> `"1,234,567"`.
fn group_digits(digits: &str) -> String {
    let mut out = String::new();
    for (i, ch) in digits.char_indices() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use nfl_data::{Season, SeasonType, TeamAbbr as NflTeamAbbr, Week};

    #[test]
    fn winning_score_is_the_maximum_above_zero() {
        assert_eq!(winning_score(vec![3.0, 17.5, 9.0]), Some(17.5));
        assert_eq!(winning_score(vec![4.25]), Some(4.25));
        assert_eq!(winning_score(Vec::<f64>::new()), None);
        assert_eq!(winning_score(vec![0.0, 0.0]), None);
        assert_eq!(winning_score(vec![-8.0, -2.0]), None);
    }

    #[test]
    fn score_line_label_and_points_render() {
        let l = line("RuTD", 2.0, 12.0);
        assert_eq!(l.label(), "2 RuTD");
        assert_eq!(l.points_str(), "12.00");
        let bonus = threshold("100+ RuY Gm", 3.0);
        assert_eq!(bonus.label(), "100+ RuY Gm");
    }

    #[test]
    fn format_points_groups_signs_and_rounds() {
        assert_eq!(format_points(1247.5), "1,247.50");
        assert_eq!(format_points(-3.2), "-3.20");
        assert_eq!(format_points(9.999), "10.00");
        assert_eq!(format_points(1234567.0), "1,234,567.00");
    }

    #[test]
    fn format_thousands_groups_and_signs() {
        assert_eq!(format_thousands(700), "700");
        assert_eq!(format_thousands(5700), "5,700");
        assert_eq!(format_thousands(-1234567), "-1,234,567");
    }

    #[test]
    fn two_scores_tie_exactly_when_they_display_the_same() {
        // The tie rule and the display rule are the same rounding, so a
        // co-winner is credited if and only if the page shows equal figures.
        for (a, b) in [(122.26, 122.26 + 1e-12), (9.999, 10.0), (0.0, -0.001)] {
            assert_eq!(ties(a, b), format_points(a) == format_points(b), "{a} {b}");
        }
    }

    #[test]
    fn ties_ignores_differences_below_display_precision() {
        // The same stat line summed in a different order can land a bit apart.
        assert!(ties(122.26, 122.26 + 1e-12));
        assert!(ties(0.0, 0.0));
        assert!(!ties(122.26, 122.27));
        assert!(!ties(10.0, 90.0));
    }

    fn zero_player() -> NflPlayerWeekStats {
        NflPlayerWeekStats {
            season: Season(2025),
            week: Week(5),
            season_type: SeasonType::Reg,
            gsis_id: "00-1".into(),
            team: NflTeamAbbr("SF".into()),
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
            fg_made_0_19: 0,
            fg_made_20_29: 0,
            fg_made_30_39: 0,
            fg_made_40_49: 0,
            fg_made_50_59: 0,
            fg_made_60_plus: 0,
            fg_missed: 0,
            pat_made: 0,
            pat_missed: 0,
            fantasy_points: 0.0,
            fantasy_points_ppr: 0.0,
        }
    }

    #[test]
    fn stat_line_renders_counted_stats_and_threshold_bonuses() {
        let mut s = zero_player();
        s.passing_yards = 325;
        assert_eq!(score_player(&s).stat_line(), "325 PaY, 300+ PaY Gm");
        let mut d = zero_defense();
        d.points_allowed = 3;
        assert_eq!(score_defense(&d).stat_line(), "3 DE/PA");
    }

    #[test]
    fn qb_with_interception_and_bonus() {
        let mut s = zero_player();
        s.passing_yards = 325; // 13.0 + 3.0 bonus
        s.passing_tds = 3; // 12.0
        s.passing_interceptions = 1; // -1.0
        let score = score_player(&s);
        assert!((score.total - (13.0 + 3.0 + 12.0 - 1.0)).abs() < 1e-9);
    }

    #[test]
    fn fumble_recovery_td_scores_six() {
        // A skill player who recovers a fumble and scores (FanDuel FU/TD),
        // like Woody Marks week 15: 30 rush yds, 1 rec / 8 yds, and the FU/TD.
        let mut s = zero_player();
        s.rushing_yards = 30; // 3.0
        s.receptions = 1; // 0.5
        s.receiving_yards = 8; // 0.8
        s.fumble_recovery_tds = 1; // 6.0
        let score = score_player(&s);
        assert!((score.total - (3.0 + 0.5 + 0.8 + 6.0)).abs() < 1e-9);
    }

    #[test]
    fn half_ppr_receptions() {
        let mut s = zero_player();
        s.receptions = 10; // 5.0
        s.receiving_yards = 100; // 10.0 + 3.0 bonus
        s.receiving_tds = 1; // 6.0
        let score = score_player(&s);
        assert!((score.total - (5.0 + 10.0 + 3.0 + 6.0)).abs() < 1e-9);
    }

    fn zero_defense() -> NflTeamWeekStats {
        NflTeamWeekStats {
            season: Season(2025),
            week: Week(5),
            season_type: SeasonType::Reg,
            team: NflTeamAbbr("SF".into()),
            opponent: NflTeamAbbr("LA".into()),
            gsis_game_id: "2025_05_SF_LA".into(),
            sacks: 0,
            interceptions: 0,
            fumble_recoveries: 0,
            safeties: 0,
            touchdowns: 0,
            blocked_kicks: 0,
            conversion_returns: 0,
            points_allowed: 10,
        }
    }

    fn test_lineup() -> Lineup {
        Lineup {
            qb: NflPlayerId("00-QB".into()),
            rb1: NflPlayerId("00-RB1".into()),
            rb2: NflPlayerId("00-RB2".into()),
            wr1: NflPlayerId("00-WR1".into()),
            wr2: NflPlayerId("00-WR2".into()),
            wr3: NflPlayerId("00-WR3".into()),
            te: NflPlayerId("00-TE".into()),
            flex: NflPlayerId("00-FLEX".into()),
            def: NflTeamAbbr("KC".into()),
        }
    }

    #[test]
    fn score_lineup_sums_players_and_defense_and_zeroes_missing_stat_lines() {
        // RB1 with 100 rushing yards: 10.0 + 3.0 bonus = 13.0.
        let mut rb = zero_player();
        rb.gsis_id = "00-RB1".into();
        rb.rushing_yards = 100;
        // Defense allowing 3 points: 7.0.
        let mut def = zero_defense();
        def.team = NflTeamAbbr("KC".into());
        def.points_allowed = 3;

        let stats = WeekStats {
            players: [(NflPlayerId("00-RB1".into()), rb)].into(),
            defenses: [(NflTeamAbbr("KC".into()), def)].into(),
        };
        // Every other slot has no stat line that week -> contributes 0.
        let total = score_lineup(&test_lineup(), &stats);
        assert!((total - (13.0 + 7.0)).abs() < 1e-9, "got {total}");
    }

    #[test]
    fn score_lineup_zeroes_missing_defense() {
        let stats = WeekStats {
            players: HashMap::new(),
            defenses: HashMap::new(),
        };
        let total = score_lineup(&test_lineup(), &stats);
        assert!(total.abs() < 1e-9, "empty stats score zero, got {total}");
    }

    #[test]
    fn defense_tier_boundaries() {
        let mut t = zero_defense();
        t.points_allowed = 6;
        assert!((score_defense(&t).total - 7.0).abs() < 1e-9);
        t.points_allowed = 7;
        assert!((score_defense(&t).total - 4.0).abs() < 1e-9);
        t.points_allowed = 0;
        assert!((score_defense(&t).total - 10.0).abs() < 1e-9);
    }

    #[test]
    fn defense_counts_every_category() {
        let mut t = zero_defense();
        t.points_allowed = 24; // 0 tier
        t.sacks = 3; // 3
        t.interceptions = 2; // 4
        t.fumble_recoveries = 1; // 2
        t.touchdowns = 1; // 6
        t.safeties = 1; // 2
        t.blocked_kicks = 1; // 2
        t.conversion_returns = 1; // 2
        assert!((score_defense(&t).total - 21.0).abs() < 1e-9);
    }
}
