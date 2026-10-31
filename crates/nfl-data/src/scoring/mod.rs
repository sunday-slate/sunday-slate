mod service;

use crate::{PlayerWeekStats as NflPlayerWeekStats, TeamAbbr, TeamWeekStats as NflTeamWeekStats};
use nfl_model::NflPlayerId;
use std::collections::HashMap;

pub(crate) use service::season_averages;

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

/// A whole-dollar figure as the reader sees it: `5700` -> `"5,700"`.
fn format_thousands(v: i64) -> String {
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
