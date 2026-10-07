use serde::Deserialize;

use crate::error::NflDataError;
use crate::ingest::{optional_espn_id, parse_csv};
use crate::model::{Season, TeamAbbr, Week, WeeklyRosterEntry};

/// nflverse roster_weekly_{season}.csv columns (subset we map). Same shape as
/// the season roster feed plus a per-row `week`.
#[derive(Debug, Deserialize)]
struct RawWeeklyRosterRow {
    season: u16,
    week: u8,
    team: String,
    position: Option<String>,
    full_name: String,
    last_name: Option<String>,
    gsis_id: Option<String>,
    espn_id: String,
}

pub(crate) fn parse(asset: &str, bytes: &[u8]) -> Result<Vec<WeeklyRosterEntry>, NflDataError> {
    parse_csv(asset, bytes, |raw: RawWeeklyRosterRow| {
        Ok(Some(WeeklyRosterEntry {
            season: Season(raw.season),
            week: Week(raw.week),
            team: TeamAbbr(raw.team),
            // Defensive: treat a blank id as unassigned regardless of how csv's
            // empty-field handling represents it, rather than relying implicitly
            // on that behavior for a business-meaningful null.
            gsis_id: raw.gsis_id.filter(|id| !id.is_empty()),
            espn_id: optional_espn_id(raw.espn_id)?,
            full_name: raw.full_name,
            last_name: raw.last_name.filter(|n| !n.is_empty()),
            position: raw.position,
        }))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Header + rows verbatim from nflverse roster_weekly_2025.csv (2026-08-02).
    // Thielen is the point of this dataset: MIN in week 13, PIT in week 14.
    const WEEKLY_ROSTER_CSV: &str = r#"season,team,position,depth_chart_position,jersey_number,status,full_name,first_name,last_name,birth_date,height,weight,college,gsis_id,espn_id,sportradar_id,yahoo_id,rotowire_id,pff_id,pfr_id,fantasy_data_id,sleeper_id,years_exp,headshot_url,ngs_position,week,game_type,status_description_abbr,football_name,esb_id,gsis_it_id,smart_id,entry_year,rookie_year,draft_club,draft_number
2025,PIT,WR,WR,16,ACT,Adam Thielen,Adam,Thielen,1990-08-22,74,200,Minnesota State,00-0030035,16460,2fa2b2da-4aa9-44b5-b27e-56876dfe2ad4,27277,8986,8288,ThieAd00,15534,1689,12,"https://static.www.nfl.com/image/upload/f_auto,q_auto/league/pv6gajxtt6zhum2unnsl",WR,14,REG,A01,Adam,THI510348,40488,32005448-4951-0348-440c-28f32f21652b,2013,2013,,
2025,MIN,WR,WR,19,INA,Adam Thielen,Adam,Thielen,1990-08-22,74,200,Minnesota State,00-0030035,16460,2fa2b2da-4aa9-44b5-b27e-56876dfe2ad4,27277,8986,8288,ThieAd00,15534,1689,12,"https://static.www.nfl.com/image/upload/f_auto,q_auto/league/pv6gajxtt6zhum2unnsl",,13,REG,A01,Adam,THI510348,40488,32005448-4951-0348-440c-28f32f21652b,2013,2013,,
2025,GB,DL,DT,69,DEV,Dante Barnett,Dante,Barnett,,,275,,,,,,,,,,,0,"https://static.www.nfl.com/image/upload/f_auto,q_auto/league/ojtce2im0wp2ltyel0vc",,16,REG,P03,Dante,BAR591037,58805,32004a55-4435-9919-355b-3e0aef2de56d,2025,2025,,
"#;

    #[test]
    fn parses_a_traded_player_onto_both_teams() {
        let entries = parse("roster_weekly_2025.csv", WEEKLY_ROSTER_CSV.as_bytes()).unwrap();
        assert_eq!(entries.len(), 3);

        let pit = &entries[0];
        assert_eq!(pit.week, Week(14));
        assert_eq!(pit.team, TeamAbbr("PIT".into()));
        assert_eq!(pit.gsis_id.as_deref(), Some("00-0030035"));
        assert_eq!(pit.position.as_deref(), Some("WR"));
        assert_eq!(pit.last_name.as_deref(), Some("Thielen"));

        // Same player, same season, earlier week, different team.
        let min = &entries[1];
        assert_eq!(min.week, Week(13));
        assert_eq!(min.team, TeamAbbr("MIN".into()));
        assert_eq!(pit.espn_id.as_deref(), Some("16460"));
        assert_eq!(min.gsis_id.as_deref(), Some("00-0030035"));
        assert_eq!(min.season, Season(2025));
    }

    #[test]
    fn treats_a_blank_gsis_id_as_unassigned() {
        let entries = parse("roster_weekly_2025.csv", WEEKLY_ROSTER_CSV.as_bytes()).unwrap();
        let barnett = &entries[2];
        assert_eq!(barnett.full_name, "Dante Barnett");
        assert_eq!(barnett.espn_id, None);
        assert_eq!(barnett.gsis_id, None);
        assert_eq!(barnett.week, Week(16));
    }

    #[test]
    fn requires_numeric_espn_id_column() {
        let malformed = "season,week,team,full_name,espn_id\n2025,1,GB,Player,N/A\n";
        let err = parse("roster_weekly_2025.csv", malformed.as_bytes()).unwrap_err();
        let rendered = err.to_string();
        assert!(
            rendered.contains("roster_weekly_2025.csv")
                && rendered.contains("row 2")
                && rendered.contains("invalid espn_id"),
            "{rendered}"
        );

        let missing_header = "season,week,team,status,full_name\n2025,1,GB,ACT,Player\n";
        let err = parse("roster_weekly_2025.csv", missing_header.as_bytes()).unwrap_err();
        assert!(err.to_string().contains("espn_id"));
    }
}
