use nfl_model::{DfsPosition, TeamAbbr};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RawSalaryRow {
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
    pub salary: String,
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

impl RawSalaryRow {
    pub fn fd_player_id(&self) -> &str {
        self.id.rsplit('-').next().unwrap_or(&self.id)
    }

    pub fn fd_list_id(&self) -> Option<&str> {
        self.id.split_once('-').map(|(list_id, _)| list_id)
    }

    pub fn name(&self) -> String {
        format!("{} {}", self.first_name, self.last_name)
    }

    /// The numeric salary FanDuel quoted, when the cell holds plain digits.
    pub fn parsed_salary(&self) -> Option<i64> {
        self.salary.parse().ok()
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

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Matchup {
    pub away: TeamAbbr,
    pub home: TeamAbbr,
}

#[derive(Debug, Clone, PartialEq)]
pub struct InterpretedRow {
    pub row_number: u64,
    pub raw: RawSalaryRow,
    pub team: TeamAbbr,
    pub position: Option<DfsPosition>,
    pub matchup: Option<Matchup>,
    pub salary: Option<i64>,
    pub diagnostics: Vec<RowDiagnostic>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RowDiagnostic {
    pub row_number: u64,
    pub field: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Interpretation {
    Empty,
    Parsed(Vec<InterpretedRow>),
    InvalidCsv(crate::CsvDiagnostic),
}
