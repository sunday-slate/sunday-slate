use serde::Deserialize;

use crate::error::NflDataError;
use crate::ingest::parse_csv;
use crate::model::{RosterEntry, Season, TeamAbbr};

/// nflverse roster_{season}.csv columns (subset we map).
#[derive(Debug, Deserialize)]
struct RawRosterRow {
    season: u16,
    team: String,
    position: Option<String>,
    jersey_number: Option<u16>,
    status: String,
    full_name: String,
    gsis_id: Option<String>,
}

pub(crate) fn parse(asset: &str, bytes: &[u8]) -> Result<Vec<RosterEntry>, NflDataError> {
    parse_csv(asset, bytes, |raw: RawRosterRow| {
        Ok(Some(RosterEntry {
            season: Season(raw.season),
            team: TeamAbbr(raw.team),
            // Defensive: treat a blank id as unassigned regardless of how csv's
            // empty-field handling represents it, rather than relying implicitly on
            // that behavior for a business-meaningful null.
            gsis_id: raw.gsis_id.filter(|id| !id.is_empty()),
            full_name: raw.full_name,
            position: raw.position,
            jersey_number: raw.jersey_number,
            status: raw.status,
        }))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Header + rows verbatim from nflverse roster_2025.csv (2026-07-01).
    const ROSTER_CSV: &str = "\
season,team,position,depth_chart_position,jersey_number,status,full_name,first_name,last_name,birth_date,height,weight,college,gsis_id,espn_id,sportradar_id,yahoo_id,rotowire_id,pff_id,pfr_id,fantasy_data_id,sleeper_id,years_exp,headshot_url,ngs_position,week,game_type,status_description_abbr,football_name,esb_id,gsis_it_id,smart_id,entry_year,rookie_year,draft_club,draft_number
2025,GB,DL,DT,69,DEV,Dante Barnett,Dante,Barnett,,,275,,\"\",,,,,,,,,0,\"https://static.www.nfl.com/image/upload/f_auto,q_auto/league/ojtce2im0wp2ltyel0vc\",,19,WC,P03,Dante,BAR591037,58805,32004a55-4435-9919-355b-3e0aef2de56d,2025,2025,,
2025,GB,QB,QB,10,ACT,Jordan Love,Jordan,Love,1998-11-02,76,220,Utah State,00-0036264,4036378,e5094779-e94f-4052-8597-bdbee3719f6b,32696,14371,40306,LoveJo03,21841,6804,5,\"https://static.www.nfl.com/image/upload/f_auto,q_auto/league/uneiwen9drvci9ahuebp\",QB,19,WC,A01,Jordan,LOV130776,52434,32004c4f-5613-0776-be4c-ce231b05c522,2020,2020,GB,26
";

    #[test]
    fn parses_roster_rows_including_missing_gsis_id() {
        let entries = parse("roster_2025.csv", ROSTER_CSV.as_bytes()).unwrap();
        assert_eq!(entries.len(), 2);

        let barnett = &entries[0];
        assert_eq!(barnett.gsis_id, None);
        assert_eq!(barnett.full_name, "Dante Barnett");
        assert_eq!(barnett.jersey_number, Some(69));
        assert_eq!(barnett.status, "DEV");

        let love = &entries[1];
        assert_eq!(love.gsis_id.as_deref(), Some("00-0036264"));
        assert_eq!(love.team, TeamAbbr("GB".into()));
        assert_eq!(love.position.as_deref(), Some("QB"));
        assert_eq!(love.jersey_number, Some(10));
    }
}
