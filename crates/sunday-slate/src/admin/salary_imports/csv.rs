use std::io::Read;

use serde::Deserialize;

use crate::player_salaries::model::DfsPosition;

/// One row of FanDuel's "Download Players List" export (NFL classic). Serde field
/// names match the real header exactly; unused columns are still deserialized so
/// the row shape lines up. DST rows use position `D`.
///
/// `id` is composite (`{fixtureListId}-{playerId}`). `game` is the directed
/// matchup `AWAY@HOME` in FanDuel's native team codes; the importer normalizes
/// those to nflverse before use.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SalaryRow {
    #[serde(rename = "Id")]
    pub id: String,
    #[serde(rename = "Position")]
    pub position: String,
    #[serde(rename = "First Name")]
    pub first_name: String,
    #[serde(rename = "Nickname")]
    pub nickname: String,
    #[serde(rename = "Last Name")]
    pub last_name: String,
    #[serde(rename = "FPPG")]
    pub fppg: String,
    #[serde(rename = "Played")]
    pub played: String,
    #[serde(rename = "Salary")]
    pub salary: i64,
    #[serde(rename = "Game")]
    pub game: String,
    #[serde(rename = "Team")]
    pub team: String,
    #[serde(rename = "Opponent")]
    pub opponent: String,
    #[serde(rename = "Injury Indicator")]
    pub injury_indicator: String,
    #[serde(rename = "Injury Details")]
    pub injury_details: String,
}

impl SalaryRow {
    /// FanDuel's per-slot player id, the crosswalk key. The export ships it
    /// composite (`{fixtureListId}-{playerId}`); the playerId suffix is what
    /// `known_good.csv` and the crosswalk join on. Tolerates a bare id.
    pub fn fd_player_id(&self) -> &str {
        self.id.rsplit('-').next().unwrap_or(&self.id)
    }

    /// Full name for player matching. `Nickname` is always blank in the export,
    /// so `First Last` reconstructs the name the matcher normalizes.
    pub fn name(&self) -> String {
        format!("{} {}", self.first_name, self.last_name)
    }

    pub fn dfs_position(&self) -> Option<DfsPosition> {
        match self.position.to_uppercase().as_str() {
            "QB" => Some(DfsPosition::Qb),
            "RB" => Some(DfsPosition::Rb),
            "WR" => Some(DfsPosition::Wr),
            "TE" => Some(DfsPosition::Te),
            "DST" | "DEF" | "D" => Some(DfsPosition::Dst),
            _ => None,
        }
    }
}

pub fn parse<R: Read>(reader: R) -> Result<Vec<SalaryRow>, csv::Error> {
    csv::Reader::from_reader(reader)
        .into_deserialize()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_real_fanduel_header() {
        let csv = "\"Id\",\"Position\",\"First Name\",\"Nickname\",\"Last Name\",\"FPPG\",\"Played\",\"Salary\",\"Game\",\"Team\",\"Opponent\",\"Injury Indicator\",\"Injury Details\"\n\
                   \"123506-62239\",\"QB\",\"Josh\",\"\",\"Allen\",\"25.5\",\"17\",\"8200\",\"BUF@NYJ\",\"BUF\",\"NYJ\",\"\",\"\"\n\
                   \"119110-12543\",\"D\",\"New York\",\"\",\"Giants\",\"4.6\",\"17\",\"3000\",\"NYG@WAS\",\"NYG\",\"WAS\",\"\",\"\"\n";
        let rows = parse(csv.as_bytes()).expect("parse");
        assert_eq!(rows.len(), 2);
        // Composite Id -> plain playerId suffix.
        assert_eq!(rows[0].fd_player_id(), "62239");
        assert_eq!(rows[1].fd_player_id(), "12543");
        // First + Last joined; Nickname ignored.
        assert_eq!(rows[0].name(), "Josh Allen");
        assert_eq!(rows[0].dfs_position(), Some(DfsPosition::Qb));
        assert_eq!(rows[1].dfs_position(), Some(DfsPosition::Dst));
        assert_eq!(rows[0].salary, 8200);
        assert_eq!(rows[0].game, "BUF@NYJ");
        assert_eq!(rows[0].team, "BUF");
    }

    #[test]
    fn fd_player_id_tolerates_a_bare_id() {
        let row = SalaryRow {
            id: "62239".into(),
            position: "QB".into(),
            first_name: "Josh".into(),
            nickname: String::new(),
            last_name: "Allen".into(),
            fppg: "0".into(),
            played: "0".into(),
            salary: 0,
            game: "BUF@NYJ".into(),
            team: "BUF".into(),
            opponent: "NYJ".into(),
            injury_indicator: String::new(),
            injury_details: String::new(),
        };
        assert_eq!(row.fd_player_id(), "62239");
    }
}
