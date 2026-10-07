use nfl_model::{DfsPosition, TeamAbbr};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SalaryRow {
    pub fd_player_id: String,
    pub name: String,
    pub team: TeamAbbr,
    pub original_position: String,
    pub position: Option<DfsPosition>,
    pub matchup: Option<SalaryMatchup>,
    pub original_salary: String,
    pub salary: Option<i64>,
    pub diagnostics: Vec<SalaryRowDiagnostic>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SalaryMatchup {
    pub away: TeamAbbr,
    pub home: TeamAbbr,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SalaryRowDiagnostic {
    pub row_number: u64,
    pub field: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SalaryCsvDiagnostic {
    pub message: String,
    pub row_number: Option<u64>,
    pub field: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SalaryUploadOutcome {
    Empty,
    Parsed(Vec<SalaryRow>),
    InvalidCsv(SalaryCsvDiagnostic),
}

#[derive(Debug, thiserror::Error)]
#[error("FanDuel salary upload failed: {0}")]
pub struct SalaryUploadError(#[from] fanduel_data::FanduelDataError);

impl From<fanduel_data::Interpretation> for SalaryUploadOutcome {
    fn from(interpretation: fanduel_data::Interpretation) -> Self {
        match interpretation {
            fanduel_data::Interpretation::Empty => Self::Empty,
            fanduel_data::Interpretation::InvalidCsv(diagnostic) => {
                Self::InvalidCsv(SalaryCsvDiagnostic {
                    message: diagnostic.message,
                    row_number: diagnostic.row_number,
                    field: diagnostic.field,
                })
            }
            fanduel_data::Interpretation::Parsed(rows) => Self::Parsed(
                rows.into_iter()
                    .map(|row| SalaryRow {
                        fd_player_id: row.raw.fd_player_id().to_owned(),
                        name: row.raw.name(),
                        team: row.team,
                        original_position: row.raw.position,
                        position: row.position,
                        matchup: row.matchup.map(|matchup| SalaryMatchup {
                            away: matchup.away,
                            home: matchup.home,
                        }),
                        original_salary: row.raw.salary,
                        salary: row.salary,
                        diagnostics: row
                            .diagnostics
                            .into_iter()
                            .map(|diagnostic| SalaryRowDiagnostic {
                                row_number: diagnostic.row_number,
                                field: diagnostic.field,
                                message: diagnostic.message,
                            })
                            .collect(),
                    })
                    .collect(),
            ),
        }
    }
}
