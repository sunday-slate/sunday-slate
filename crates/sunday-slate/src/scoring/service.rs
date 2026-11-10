//! Scoring assembly a handler can call: load the stats a set of contests
//! needs, then score each fantasy team's entries against them.

use std::collections::{HashMap, HashSet};

use nfl_data::{NflData, NflDataError, SeasonWeek};

use crate::entries::{FantasyTeamEntry, NflPlayerId};
use crate::scoring::{self, WeekStats};
use nfl_data::slates::SlateId;

/// Load all fantasy stats for completed weeks.
pub async fn week_stats(
    nfl: &NflData,
    weeks: impl IntoIterator<Item = SeasonWeek>,
) -> Result<HashMap<SeasonWeek, WeekStats>, NflDataError> {
    let mut stats: HashMap<SeasonWeek, WeekStats> = HashMap::new();
    let mut seen: HashSet<SeasonWeek> = HashSet::new();
    for week in weeks {
        if !seen.insert(week) {
            continue;
        }
        let players: HashMap<NflPlayerId, _> = nfl
            .player_week_stats(week)
            .await?
            .into_iter()
            .map(|p| (NflPlayerId(p.gsis_id.clone()), p))
            .collect();
        let defenses: HashMap<_, _> = nfl
            .team_week_stats(week)
            .await?
            .into_iter()
            .map(|t| (t.team, t))
            .collect();
        if players.is_empty() || defenses.is_empty() {
            continue;
        }
        stats.insert(week, WeekStats { players, defenses });
    }
    Ok(stats)
}

/// Score one fantasy team's entries: each contest that resolves to a week
/// with loaded stats gets a total; the rest are skipped.
pub fn score_entries(
    entries: &[FantasyTeamEntry],
    contest_week: &HashMap<SlateId, SeasonWeek>,
    stats: &HashMap<SeasonWeek, WeekStats>,
) -> Vec<(SlateId, f64)> {
    entries
        .iter()
        .filter_map(|e| {
            let week_stats = stats.get(contest_week.get(&e.slate_id)?)?;
            Some((e.slate_id, scoring::score_lineup(&e.lineup, week_stats)))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entries::Lineup;
    use crate::entries::NflPlayerId;
    use nfl_data::{
        PlayerWeekStats as NflPlayerWeekStats, Season, SeasonWeek, TeamAbbr,
        TeamWeekStats as NflTeamWeekStats, Week,
    };

    fn week(season: u16, week: u8) -> SeasonWeek {
        SeasonWeek::new(Season(season), Week(week)).expect("valid fixture week")
    }

    /// The lineup's only scoring player is `00-RB`; every other slot is a
    /// player with no stat line, so the total is RB plus DST.
    fn lineup() -> Lineup {
        let player = |id: &str| NflPlayerId(id.into());
        Lineup {
            qb: player("00-QB"),
            rb1: player("00-RB"),
            rb2: player("00-RB2"),
            wr1: player("00-WR1"),
            wr2: player("00-WR2"),
            wr3: player("00-WR3"),
            te: player("00-TE"),
            flex: player("00-FLEX"),
            def: TeamAbbr::KC,
        }
    }

    /// Every scoring count zero except receptions: the total is exactly
    /// `0.5 * receptions` with no threshold bonus.
    fn rb(season_week: SeasonWeek, receptions: u32) -> NflPlayerWeekStats {
        NflPlayerWeekStats {
            season_week,
            gsis_id: "00-RB".into(),
            team: TeamAbbr::KC,
            opponent: Some(TeamAbbr::BUF),
            completions: 0,
            attempts: 0,
            passing_yards: 0,
            passing_tds: 0,
            passing_interceptions: 0,
            rushing_attempts: 0,
            rushing_yards: 0,
            rushing_tds: 0,
            targets: 0,
            receptions,
            receiving_yards: 0,
            receiving_tds: 0,
            fumbles_lost: 0,
            two_point_conversions: 0,
            special_teams_tds: 0,
            fumble_recovery_tds: 0,
        }
    }

    fn defense(season_week: SeasonWeek, points_allowed: i32) -> NflTeamWeekStats {
        NflTeamWeekStats {
            season_week,
            team: TeamAbbr::KC,
            opponent: TeamAbbr::BUF,
            game_id: format!("{}_{:02}_BUF_KC", season_week.season, season_week.week.0),
            sacks: 0,
            interceptions: 0,
            fumble_recoveries: 0,
            safeties: 0,
            touchdowns: 0,
            blocked_kicks: 0,
            conversion_returns: 0,
            points_allowed,
        }
    }

    fn stats(season_week: SeasonWeek, receptions: u32, points_allowed: i32) -> WeekStats {
        WeekStats {
            players: [(NflPlayerId("00-RB".into()), rb(season_week, receptions))].into(),
            defenses: [(TeamAbbr::KC, defense(season_week, points_allowed))].into(),
        }
    }

    #[test]
    fn season_week_score_entries_resolve_each_slates_own_season() {
        let older = week(2025, 1);
        let newer = week(2026, 1);
        let one = SlateId(20250011);
        let two = SlateId(20260011);

        let contest_week = HashMap::from([(one, older), (two, newer)]);
        let week_stats = HashMap::from([
            // 2025: 20 receptions = 10.00, shutout = 10.00.
            (older, stats(older, 20, 0)),
            // 2026: 40 receptions = 20.00, 7 allowed = 4.00.
            (newer, stats(newer, 40, 7)),
        ]);
        let entries = [
            FantasyTeamEntry {
                slate_id: one,
                lineup: lineup(),
            },
            FantasyTeamEntry {
                slate_id: two,
                lineup: lineup(),
            },
        ];

        let mut scores = score_entries(&entries, &contest_week, &week_stats);
        scores.sort_by_key(|(slate, _)| *slate);
        assert_eq!(scores, vec![(one, 20.0), (two, 24.0)]);
    }

    #[test]
    fn season_week_score_entries_skip_a_contest_without_stats() {
        let older = week(2025, 1);
        let newer = week(2026, 1);
        let one = SlateId(20250011);
        let two = SlateId(20260011);

        let contest_week = HashMap::from([(one, older), (two, newer)]);
        let week_stats = HashMap::from([(older, stats(older, 20, 0))]);
        let entries = [
            FantasyTeamEntry {
                slate_id: one,
                lineup: lineup(),
            },
            FantasyTeamEntry {
                slate_id: two,
                lineup: lineup(),
            },
        ];

        assert_eq!(
            score_entries(&entries, &contest_week, &week_stats),
            vec![(one, 20.0)],
            "the 2026 contest has no loaded stats and is skipped"
        );
    }
}
