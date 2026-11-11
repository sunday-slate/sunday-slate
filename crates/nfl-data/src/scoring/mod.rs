mod service;

use crate::{PlayerWeekStats as NflPlayerWeekStats, TeamAbbr, TeamWeekStats as NflTeamWeekStats};
use nfl_model::NflPlayerId;
use std::collections::HashMap;

pub use service::{SeasonAverages, season_averages, week_stats};

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

/// Season-to-date FPPG
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct PointsPerGame {
    pub avg_points: f64,
    pub games_played: u8,
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
/// every FanDuel DST category is covered.
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
    pub defenses: HashMap<TeamAbbr, NflTeamWeekStats>,
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
fn format_points(points: f64) -> String {
    let hundredths = hundredths(points);
    let sign = if hundredths < 0 { "-" } else { "" };
    let hundredths = hundredths.unsigned_abs();
    let whole = group_digits(&(hundredths / 100).to_string());
    let frac = hundredths % 100;
    format!("{sign}{whole}.{frac:02}")
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
    use crate::{Season, SeasonWeek, Week};
    fn zero_player() -> NflPlayerWeekStats {
        NflPlayerWeekStats {
            season_week: SeasonWeek::new(Season(2025), Week(5)).expect("valid fixture"),
            gsis_id: "00-1".into(),
            team: TeamAbbr::SF,
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
            season_week: SeasonWeek::new(Season(2025), Week(5)).expect("valid fixture"),
            team: TeamAbbr::SF,
            opponent: TeamAbbr::LA,
            game_id: "2025_05_SF_LA".into(),
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
