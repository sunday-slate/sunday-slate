use nfl_model::{DfsPosition, TeamAbbr};
use serde::{Deserialize, de::Visitor};
use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub struct RawSalaryRow {
    pub id: String,
    pub position: String,
    pub first_name: String,
    pub nickname: String,
    pub last_name: String,
    pub fppg: String,
    pub played: String,
    pub salary: i64,
    pub quoted_salary: String,
    pub game: String,
    pub team: String,
    pub opponent: String,
    pub injury_indicator: String,
    pub injury_details: String,
}

impl<'de> Deserialize<'de> for RawSalaryRow {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let fields = RawSalaryRowFields::deserialize(deserializer)?;
        Ok(Self {
            id: fields.id,
            position: fields.position,
            first_name: fields.first_name,
            nickname: fields.nickname,
            last_name: fields.last_name,
            fppg: fields.fppg,
            played: fields.played,
            salary: fields.salary.value,
            quoted_salary: fields.salary.text,
            game: fields.game,
            team: fields.team,
            opponent: fields.opponent,
            injury_indicator: fields.injury_indicator,
            injury_details: fields.injury_details,
        })
    }
}

#[derive(Deserialize)]
struct RawSalaryRowFields {
    #[serde(rename = "Id")]
    id: String,
    #[serde(rename = "Position")]
    position: String,
    #[serde(rename = "First Name")]
    first_name: String,
    #[serde(rename = "Nickname")]
    nickname: String,
    #[serde(rename = "Last Name")]
    last_name: String,
    #[serde(rename = "FPPG")]
    fppg: String,
    #[serde(rename = "Played")]
    played: String,
    #[serde(rename = "Salary")]
    salary: SalaryValue,
    #[serde(rename = "Game")]
    game: String,
    #[serde(rename = "Team")]
    team: String,
    #[serde(rename = "Opponent")]
    opponent: String,
    #[serde(rename = "Injury Indicator")]
    injury_indicator: String,
    #[serde(rename = "Injury Details")]
    injury_details: String,
}

struct SalaryValue {
    value: i64,
    text: String,
}

impl<'de> Deserialize<'de> for SalaryValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct SalaryVisitor;

        impl Visitor<'_> for SalaryVisitor {
            type Value = SalaryValue;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("an integer salary")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                let numeric = value
                    .parse()
                    .map_err(|error| E::custom(format!("FanDuel Salary: {error}")))?;
                Ok(SalaryValue {
                    value: numeric,
                    text: value.to_owned(),
                })
            }

            fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(SalaryValue {
                    value,
                    text: value.to_string(),
                })
            }
        }

        deserializer.deserialize_str(SalaryVisitor)
    }
}

impl serde::Serialize for RawSalaryRow {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;

        let mut row = serializer.serialize_struct("RawSalaryRow", 13)?;
        row.serialize_field("Id", &self.id)?;
        row.serialize_field("Position", &self.position)?;
        row.serialize_field("First Name", &self.first_name)?;
        row.serialize_field("Nickname", &self.nickname)?;
        row.serialize_field("Last Name", &self.last_name)?;
        row.serialize_field("FPPG", &self.fppg)?;
        row.serialize_field("Played", &self.played)?;
        row.serialize_field("Salary", &self.quoted_salary)?;
        row.serialize_field("Game", &self.game)?;
        row.serialize_field("Team", &self.team)?;
        row.serialize_field("Opponent", &self.opponent)?;
        row.serialize_field("Injury Indicator", &self.injury_indicator)?;
        row.serialize_field("Injury Details", &self.injury_details)?;
        row.end()
    }
}

impl RawSalaryRow {
    pub fn fd_player_id(&self) -> &str {
        self.id.rsplit('-').next().unwrap_or(&self.id)
    }

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
