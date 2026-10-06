use std::collections::BTreeMap;

use serde::Deserialize;

use crate::error::NflDataError;
use crate::ingest::{parse_csv, parse_season_type};
use crate::model::{Season, SeasonType, TeamAbbr, TeamWeekStats, Week};

/// pbp columns we read. serde-csv matches by header name and ignores the
/// feed's other ~350 columns. Counting flags arrive as "1"/"0"/"" floats.
#[derive(Debug, Deserialize)]
struct RawPlay {
    game_id: String,
    season: u16,
    week: u8,
    season_type: String,
    home_team: String,
    away_team: String,
    posteam: Option<String>,
    defteam: Option<String>,
    td_team: Option<String>,
    fumbled_1_team: Option<String>,
    fumble_recovery_1_team: Option<String>,
    fumbled_2_team: Option<String>,
    fumble_recovery_2_team: Option<String>,
    fumble_lost: Option<f64>,
    sack: Option<f64>,
    interception: Option<f64>,
    touchdown: Option<f64>,
    return_touchdown: Option<f64>,
    own_kickoff_recovery_td: Option<f64>,
    safety: Option<f64>,
    punt_blocked: Option<f64>,
    field_goal_result: Option<String>,
    extra_point_result: Option<String>,
    two_point_conv_result: Option<String>,
    defensive_extra_point_conv: Option<f64>,
    defensive_two_point_conv: Option<f64>,
}

fn flag(v: Option<f64>) -> bool {
    v.unwrap_or(0.0) >= 0.5
}

/// The other team in the game, for crediting a takeaway to the side that did
/// not fumble.
fn other_team<'a>(game: &'a GameAccum, team: &str) -> Option<&'a str> {
    if team == game.home {
        Some(&game.away)
    } else if team == game.away {
        Some(&game.home)
    } else {
        None
    }
}

#[derive(Default)]
struct Accum {
    sacks: u32,
    interceptions: u32,
    fumble_recoveries: u32,
    safeties: u32,
    touchdowns: u32,
    blocked_kicks: u32,
    conversion_returns: u32,
    /// This team's own offensive points; becomes the opponent's points_allowed.
    offensive_points: i32,
}

struct GameAccum {
    home: String,
    away: String,
    week: u8,
    season: u16,
    season_type: SeasonType,
    teams: BTreeMap<String, Accum>,
}

/// Aggregate one season's play-by-play into per-team-week D/ST rows.
pub(crate) fn parse(asset: &str, bytes: &[u8]) -> Result<Vec<TeamWeekStats>, NflDataError> {
    // Keyed by game_id; each game accumulates both teams. parse_csv gives strict
    // row errors (a malformed play fails the asset, as with every other feed).
    let plays = parse_csv(asset, bytes, |raw: RawPlay| {
        let season_type = parse_season_type(&raw.season_type)?;
        Ok(Some((raw, season_type)))
    })?;

    let mut games: BTreeMap<String, GameAccum> = BTreeMap::new();
    for (p, season_type) in plays {
        let game = games.entry(p.game_id.clone()).or_insert_with(|| GameAccum {
            home: p.home_team.clone(),
            away: p.away_team.clone(),
            week: p.week,
            season: p.season,
            season_type,
            teams: BTreeMap::new(),
        });
        game.teams.entry(game.home.clone()).or_default();
        game.teams.entry(game.away.clone()).or_default();

        // Defense-credited counting stats (also credits the kicking team on a
        // kickoff, which nflverse sets as defteam).
        if let Some(def) = p.defteam.as_deref()
            && let Some(a) = game.teams.get_mut(def)
        {
            if flag(p.sack) {
                a.sacks += 1;
            }
            if flag(p.interception) {
                a.interceptions += 1;
            }
            if flag(p.safety) {
                a.safeties += 1;
            }
            if flag(p.punt_blocked)
                || p.field_goal_result.as_deref() == Some("blocked")
                || p.extra_point_result.as_deref() == Some("blocked")
            {
                a.blocked_kicks += 1;
            }
            if flag(p.defensive_extra_point_conv) || flag(p.defensive_two_point_conv) {
                a.conversion_returns += 1;
            }
            // A muffed kick the kicking team recovers for a TD counts as a
            // fumble recovery (FanDuel credits FR + the return TD). A plain
            // own_kickoff_recovery — a recovered ONSIDE kick — is not a fumble
            // recovery, so key on the _td flag, not own_kickoff_recovery. On
            // these plays the generic fumbled_1/fumble_recovery_1 columns are
            // empty (verified against the 2025 SEA muffed-kickoff TD), so the
            // first-fumble block below does not also credit this recovery.
            if flag(p.own_kickoff_recovery_td) {
                a.fumble_recoveries += 1;
            }
        }

        // A takeaway on the first fumble: either the other team recovered it,
        // or the fumbling team lost it with no recoverer recorded (fumbled out
        // of bounds / into the end zone for a touchback) — FanDuel credits the
        // D/ST a fumble recovery either way.
        if let Some(lost) = p.fumbled_1_team.as_deref() {
            let taker: Option<String> = match p.fumble_recovery_1_team.as_deref() {
                Some(rec) if rec != lost => Some(rec.to_string()),
                Some(_) => None, // the fumbling team recovered its own — no takeaway
                None if flag(p.fumble_lost) => other_team(game, lost).map(str::to_string),
                None => None,
            };
            if let Some(t) = taker
                && let Some(a) = game.teams.get_mut(&t)
            {
                a.fumble_recoveries += 1;
            }
        }

        // A second fumble on the same play (e.g. a sack-fumble after the QB
        // recovered his own first fumble): the recovering team took the ball
        // away from whoever is in fumbled_2_team, or fumbled_1_team when
        // upstream leaves the second fumbler blank.
        if let Some(rec) = p.fumble_recovery_2_team.as_deref()
            && let Some(lost) = p.fumbled_2_team.as_deref().or(p.fumbled_1_team.as_deref())
            && lost != rec
            && let Some(a) = game.teams.get_mut(rec)
        {
            a.fumble_recoveries += 1;
        }

        // Touchdowns: offensive (into the scorer's offense, feeding the
        // opponent's points_allowed) vs non-offensive (D/ST +6).
        if flag(p.touchdown)
            && let Some(scorer) = p.td_team.as_deref()
            && let Some(a) = game.teams.get_mut(scorer)
        {
            let offensive = p.posteam.as_deref() == Some(scorer)
                && !flag(p.return_touchdown)
                && !flag(p.own_kickoff_recovery_td);
            if offensive {
                a.offensive_points += 6;
            } else {
                a.touchdowns += 1;
            }
        }

        // Offensive kicking/2pt points feed the opponent's points_allowed.
        if let Some(pos) = p.posteam.as_deref()
            && let Some(a) = game.teams.get_mut(pos)
        {
            if p.field_goal_result.as_deref() == Some("made") {
                a.offensive_points += 3;
            }
            if p.extra_point_result.as_deref() == Some("good") {
                a.offensive_points += 1;
            }
            if p.two_point_conv_result.as_deref() == Some("success") {
                a.offensive_points += 2;
            }
        }
    }

    let mut out = Vec::new();
    for (game_id, g) in games {
        for team in [&g.home, &g.away] {
            let opponent = if team == &g.home { &g.away } else { &g.home };
            let a = &g.teams[team];
            let opp = &g.teams[opponent];
            out.push(TeamWeekStats {
                season: Season(g.season),
                week: Week(g.week),
                season_type: g.season_type,
                team: TeamAbbr(team.clone()),
                opponent: TeamAbbr(opponent.clone()),
                gsis_game_id: game_id.clone(),
                sacks: a.sacks,
                interceptions: a.interceptions,
                fumble_recoveries: a.fumble_recoveries,
                safeties: a.safeties,
                touchdowns: a.touchdowns,
                blocked_kicks: a.blocked_kicks,
                conversion_returns: a.conversion_returns,
                points_allowed: opp.offensive_points,
            });
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PBP: &str = include_str!("../../tests/fixtures/play_by_play_sample.csv");

    fn find<'a>(rows: &'a [TeamWeekStats], team: &str) -> &'a TeamWeekStats {
        rows.iter().find(|r| r.team.0 == team).expect("team row")
    }

    #[test]
    fn aggregates_one_game_to_two_team_rows() {
        let rows = parse("play_by_play_2025.csv", PBP.as_bytes()).unwrap();
        assert_eq!(rows.len(), 2);

        let aaa = find(&rows, "AAA");
        assert_eq!(aaa.opponent, TeamAbbr("BBB".into()));
        assert_eq!(aaa.gsis_game_id, "2025_01_AAA_BBB");
        assert_eq!(aaa.sacks, 1);
        assert_eq!(aaa.interceptions, 1);
        assert_eq!(aaa.fumble_recoveries, 1);
        assert_eq!(aaa.safeties, 0);
        assert_eq!(aaa.touchdowns, 2); // pick-six + strip-sack return
        assert_eq!(aaa.blocked_kicks, 0);
        assert_eq!(aaa.conversion_returns, 1);
        assert_eq!(aaa.points_allowed, 10); // BBB offense: TD+XP(7) + FG(3)

        let bbb = find(&rows, "BBB");
        assert_eq!(bbb.safeties, 1);
        assert_eq!(bbb.blocked_kicks, 1);
        assert_eq!(bbb.touchdowns, 0);
        assert_eq!(bbb.points_allowed, 0); // AAA scored only non-offensively
    }

    #[test]
    fn fr_counts_muffed_kick_second_recovery_and_touchback_but_not_onside() {
        // CCC gets a fumble recovery from each of: a muffed kickoff returned for
        // a TD (FanDuel: FR + return TD), a sack-fumble that appears as the
        // play's SECOND recovery after the QB re-grabbed his first fumble, and a
        // DDD fumble lost out of the end zone for a touchback (no recoverer
        // recorded). DDD re-grabbing its own first fumble is not a takeaway, and
        // a kickoff without own_kickoff_recovery_td (an onside recovery) is not
        // a fumble recovery.
        let csv = "game_id,season,week,season_type,home_team,away_team,posteam,defteam,td_team,fumbled_1_team,fumble_recovery_1_team,fumbled_2_team,fumble_recovery_2_team,fumble_lost,sack,interception,touchdown,return_touchdown,own_kickoff_recovery_td,safety,punt_blocked,field_goal_result,extra_point_result,two_point_conv_result,defensive_extra_point_conv,defensive_two_point_conv\n\
2025_01_DDD_CCC,2025,1,REG,CCC,DDD,DDD,CCC,CCC,,,,,0,0,0,1,0,1,0,0,,,,0,0\n\
2025_01_DDD_CCC,2025,1,REG,CCC,DDD,DDD,CCC,,DDD,DDD,,CCC,0,1,0,0,0,0,0,0,,,,0,0\n\
2025_01_DDD_CCC,2025,1,REG,CCC,DDD,DDD,CCC,,,,,,0,0,0,0,0,0,0,0,,,,0,0\n\
2025_01_DDD_CCC,2025,1,REG,CCC,DDD,DDD,CCC,,DDD,,,,1,0,0,0,0,0,0,0,,,,0,0\n";
        let rows = parse("play_by_play_2025.csv", csv.as_bytes()).unwrap();
        let ccc = find(&rows, "CCC");
        assert_eq!(ccc.fumble_recoveries, 3); // muffed-kick TD + 2nd recovery + touchback
        assert_eq!(ccc.touchdowns, 1); // the muffed-kick return TD
        assert_eq!(ccc.sacks, 1);
        let ddd = find(&rows, "DDD");
        assert_eq!(ddd.fumble_recoveries, 0); // own-fumble re-grab; onside is not an FR
    }
}
