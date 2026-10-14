use crate::FanduelDataError;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub(super) struct SalaryRow {
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

// Take a CSV file and parse to rows
pub(super) fn parse_rows(data: &[u8]) -> Result<Vec<SalaryRow>, FanduelDataError> {
    let mut reader = csv::Reader::from_reader(data);
    reader
        .deserialize()
        .collect::<Result<Vec<_>, _>>()
        .map_err(FanduelDataError::Csv)
}
