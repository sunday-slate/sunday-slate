use serde::Deserialize;

use crate::FanduelDataError;

#[derive(Debug, Deserialize)]
pub(crate) struct SalaryRow {
    #[serde(rename = "Id")]
    pub id: String,
    #[serde(rename = "Position")]
    pub position: String,
    #[serde(rename = "First Name")]
    pub first_name: String,
    #[serde(rename = "Last Name")]
    pub last_name: String,
    #[serde(rename = "Salary")]
    pub salary: String,
    #[serde(rename = "Game")]
    pub game: String,
    #[serde(rename = "Team")]
    pub team: String,
}

impl SalaryRow {
    pub fn position(&self) -> Option<nfl_model::DfsPosition> {
        match self.position.to_ascii_uppercase().as_str() {
            "QB" => Some(nfl_model::DfsPosition::Qb),
            "RB" => Some(nfl_model::DfsPosition::Rb),
            "WR" => Some(nfl_model::DfsPosition::Wr),
            "TE" => Some(nfl_model::DfsPosition::Te),
            "DST" | "DEF" | "D" => Some(nfl_model::DfsPosition::Dst),
            _ => None,
        }
    }
}

pub(crate) fn parse(data: &[u8]) -> Result<Vec<SalaryRow>, FanduelDataError> {
    let mut reader = csv::Reader::from_reader(data);
    reader
        .deserialize()
        .collect::<Result<Vec<_>, _>>()
        .map_err(FanduelDataError::Csv)
}
