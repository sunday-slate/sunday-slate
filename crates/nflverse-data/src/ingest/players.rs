use serde::Deserialize;

use crate::error::NflDataError;
use crate::ingest::{optional_espn_id, parse_csv};
use crate::model::{Player, TeamAbbr};

/// nflverse players.csv columns (subset we map).
#[derive(Debug, Deserialize)]
struct RawPlayer {
    gsis_id: Option<String>,
    espn_id: String,
    display_name: String,
    first_name: Option<String>,
    last_name: Option<String>,
    position: Option<String>,
    latest_team: Option<String>,
    headshot: Option<String>,
}

pub(crate) fn parse(asset: &str, bytes: &[u8]) -> Result<Vec<Player>, NflDataError> {
    parse_csv(asset, bytes, |raw: RawPlayer| {
        // gsis_id keys this dataset and joins it to rosters/stats; verified
        // always present upstream — treat absence as malformed, not skippable.
        let gsis_id = raw.gsis_id.ok_or_else(|| "missing gsis_id".to_string())?;
        Ok(Some(Player {
            gsis_id,
            espn_id: optional_espn_id(raw.espn_id)?,
            full_name: raw.display_name,
            first_name: raw.first_name,
            last_name: raw.last_name,
            position: raw.position,
            latest_team: raw.latest_team.map(TeamAbbr),
            headshot_url: raw.headshot,
        }))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Header + rows verbatim from nflverse players.csv (2026-07-01).
    const PLAYERS_CSV: &str = "\
gsis_id,display_name,common_first_name,first_name,last_name,short_name,football_name,suffix,esb_id,nfl_id,pfr_id,pff_id,otc_id,espn_id,smart_id,birth_date,position_group,position,ngs_position_group,ngs_position,height,weight,headshot,college_name,college_conference,jersey_number,rookie_season,last_season,latest_team,status,ngs_status,ngs_status_short_description,years_of_experience,pff_position,pff_status,draft_year,draft_round,draft_pick,draft_team
00-0028830,Isaako Aaitui,Isaako,Isaako,Aaitui,,,,AAI622937,,AaitIs00,6998,2535,14856,32004141-4962-2937-61ff-017b1804dec6,1987-01-25,DL,NT,,,76,307,\"https://static.www.nfl.com/image/private/f_auto,q_auto/league/hwncbbaztu3pc5unqgnj\",UNLV,,0,2011,2014,WAS,DEV,,,2,DI,,,,,
00-0036264,Jordan Love,Jordan,Jordan,Love,J.Love,Jordan,,LOV130776,52434,LoveJo03,40306,8766,4036378,32004c4f-5613-0776-be4c-ce231b05c522,1998-11-02,QB,QB,,,76,219,\"https://static.www.nfl.com/image/upload/f_auto,q_auto/league/uneiwen9drvci9ahuebp\",Utah State,Mountain West Conference,10,2020,2026,GB,ACT,ACT,Active,7,QB,A,2020,1,26,GB
";

    #[test]
    fn parses_players() {
        let players = parse("players.csv", PLAYERS_CSV.as_bytes()).unwrap();
        assert_eq!(players.len(), 2);

        let love = &players[1];
        assert_eq!(love.gsis_id, "00-0036264");
        assert_eq!(love.full_name, "Jordan Love");
        assert_eq!(love.espn_id.as_deref(), Some("4036378"));
        assert_eq!(love.position.as_deref(), Some("QB"));
        assert_eq!(love.latest_team, Some(TeamAbbr("GB".into())));
        assert!(
            love.headshot_url
                .as_deref()
                .unwrap()
                .starts_with("https://")
        );
    }

    #[test]
    fn requires_numeric_espn_id_column() {
        let malformed = "gsis_id,display_name,espn_id\n00-0000001,Player,N/A\n";
        let err = parse("players.csv", malformed.as_bytes()).unwrap_err();
        let rendered = err.to_string();
        assert!(
            rendered.contains("players.csv")
                && rendered.contains("row 2")
                && rendered.contains("invalid espn_id"),
            "{rendered}"
        );

        let missing_header = "gsis_id,display_name\n00-0000001,Player\n";
        let err = parse("players.csv", missing_header.as_bytes()).unwrap_err();
        assert!(err.to_string().contains("espn_id"));
    }
}
