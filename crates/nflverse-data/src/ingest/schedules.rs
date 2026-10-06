use serde::Deserialize;
use time::macros::format_description;
use time::{Date, PrimitiveDateTime, Time, UtcOffset};

use crate::error::NflDataError;
use crate::ingest::{parse_csv, parse_season_type};
use crate::model::{Game, Season, TeamAbbr, Week};

/// nflverse games.csv columns (subset we map). Names must match upstream.
#[derive(Debug, Deserialize)]
struct RawGame {
    game_id: String,
    season: u16,
    game_type: String,
    week: u8,
    gameday: String,
    gametime: Option<String>,
    away_team: String,
    away_score: Option<i32>,
    home_team: String,
    home_score: Option<i32>,
}

pub(crate) fn parse(
    asset: &str,
    bytes: &[u8],
    earliest_season: u16,
) -> Result<Vec<Game>, NflDataError> {
    parse_csv(asset, bytes, |raw: RawGame| {
        if raw.season < earliest_season {
            return Ok(None);
        }
        let season_type = parse_season_type(&raw.game_type)?;
        let gameday = Date::parse(&raw.gameday, format_description!("[year]-[month]-[day]"))
            .map_err(|e| format!("bad gameday {:?}: {e}", raw.gameday))?;
        let kickoff = match raw.gametime.as_deref() {
            None | Some("") => None,
            Some(t) => {
                let hm = Time::parse(t, format_description!("[hour]:[minute]"))
                    .map_err(|e| format!("bad gametime {t:?}: {e}"))?;
                // Kickoff times are published in US Eastern. The offset is
                // resolved by date; no NFL game kicks off inside the 2 AM
                // transition window.
                Some(
                    PrimitiveDateTime::new(gameday, hm)
                        .assume_offset(crate::eastern::eastern_offset(gameday))
                        .to_offset(UtcOffset::UTC),
                )
            }
        };
        Ok(Some(Game {
            gsis_game_id: raw.game_id,
            season: Season(raw.season),
            week: Week(raw.week),
            season_type,
            kickoff,
            home_team: TeamAbbr(raw.home_team),
            away_team: TeamAbbr(raw.away_team),
            home_score: raw.home_score,
            away_score: raw.away_score,
        }))
    })
}

#[cfg(test)]
mod tests {
    use time::macros::{date, datetime};

    use super::*;
    use crate::model::{Season, SeasonType, Week};

    // Header + rows verbatim from nflverse games.csv (2026-07-01).
    const GAMES_CSV: &str = "\
game_id,season,game_type,week,gameday,weekday,gametime,away_team,away_score,home_team,home_score,location,result,total,overtime,old_game_id,gsis,nfl_detail_id,pfr,pff,espn,ftn,away_rest,home_rest,away_moneyline,home_moneyline,spread_line,away_spread_odds,home_spread_odds,total_line,under_odds,over_odds,div_game,roof,surface,temp,wind,away_qb_id,home_qb_id,away_qb_name,home_qb_name,away_coach,home_coach,referee,stadium_id,stadium
1999_01_MIN_ATL,1999,REG,1,1999-09-12,Sunday,,MIN,17,ATL,14,Home,-3,31,0,1999091210,598,,199909120atl,,190912001,,7,7,,,-4,,,49,,,0,dome,astroturf,,,00-0003761,00-0002876,Randall Cunningham,Chris Chandler,Dennis Green,Dan Reeves,Gerry Austin,ATL00,Georgia Dome
2025_05_SF_LA,2025,REG,5,2025-10-02,Thursday,20:15,SF,26,LA,23,Home,-3,49,1,2025100200,59907,,202510020ram,28482,401772939,6798,4,4,340,-440,8.5,-110,-110,43.5,-108,-112,1,dome,matrixturf,,,00-0036972,00-0026498,Mac Jones,Matthew Stafford,Kyle Shanahan,Sean McVay,Bill Vinovich,LAX01,SoFi Stadium
2025_22_SEA_NE,2025,SB,22,2026-02-08,Sunday,18:30,SEA,29,NE,13,Neutral,-16,42,0,2026020800,60176,,202602080nwe,,401772988,,14,14,-238,195,-4.5,-115,-105,45.5,-115,-105,0,outdoors,grass,67,7,00-0034869,00-0039851,Sam Darnold,Drake Maye,Mike Macdonald,Mike Vrabel,Shawn Smith,SFO01,Levi's Stadium
";

    #[test]
    fn parses_games_in_window_and_converts_kickoff_to_utc() {
        let games = parse("games.csv", GAMES_CSV.as_bytes(), 2025).unwrap();

        assert_eq!(games.len(), 2); // the 1999 row is outside the window

        let thursday = &games[0];
        assert_eq!(thursday.gsis_game_id, "2025_05_SF_LA");
        assert_eq!(thursday.season, Season(2025));
        assert_eq!(thursday.week, Week(5));
        assert_eq!(thursday.season_type, SeasonType::Reg);
        // 2025-10-02 20:15 America/New_York (EDT, -4) == 00:15 UTC next day.
        assert_eq!(thursday.kickoff, Some(datetime!(2025-10-03 00:15 UTC)));
        assert_eq!(thursday.home_team.0, "LA");
        assert_eq!(thursday.away_team.0, "SF");
        assert_eq!(thursday.home_score, Some(23));
        assert_eq!(thursday.away_score, Some(26));

        let super_bowl = &games[1];
        assert_eq!(super_bowl.season_type, SeasonType::Post);
        // 2026-02-08 18:30 America/New_York (EST, -5) == 23:30 UTC.
        assert_eq!(super_bowl.kickoff, Some(datetime!(2026-02-08 23:30 UTC)));
    }

    #[test]
    fn empty_gametime_gives_null_kickoff() {
        let games = parse("games.csv", GAMES_CSV.as_bytes(), 1999).unwrap();
        assert_eq!(games[0].gsis_game_id, "1999_01_MIN_ATL");
        assert_eq!(games[0].kickoff, None);
    }

    #[test]
    fn unknown_game_type_fails_the_asset_with_row_number() {
        let csv = "game_id,season,game_type,week,gameday,weekday,gametime,away_team,away_score,home_team,home_score\n\
2025_01_A_B,2025,BOWL,1,2025-09-07,Sunday,13:00,A,,B,\n";
        let err = parse("games.csv", csv.as_bytes(), 2025).unwrap_err();
        let rendered = err.to_string();
        assert!(rendered.contains("games.csv"), "{rendered}");
        assert!(rendered.contains("row 2"), "{rendered}");
    }

    #[test]
    fn us_eastern_dst_boundaries() {
        use crate::eastern::eastern_offset;

        // 2026: DST starts Sun Mar 8, ends Sun Nov 1.
        assert_eq!(eastern_offset(date!(2026 - 03 - 07)).whole_hours(), -5);
        assert_eq!(eastern_offset(date!(2026 - 03 - 08)).whole_hours(), -4);
        // 2025: DST ends Sun Nov 2.
        assert_eq!(eastern_offset(date!(2025 - 11 - 01)).whole_hours(), -4);
        assert_eq!(eastern_offset(date!(2025 - 11 - 02)).whole_hours(), -5);
    }
}
