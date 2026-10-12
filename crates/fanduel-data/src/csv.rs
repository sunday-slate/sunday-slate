use serde::Deserialize;

use crate::{FanduelDataError, Game, Matchup, Player, SalariedPlayer};
use nfl_model::{DfsPosition, SeasonWeek, TeamAbbr};

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

fn parse_rows(data: &[u8]) -> Result<Vec<SalaryRow>, FanduelDataError> {
    let mut reader = csv::Reader::from_reader(data);
    reader
        .deserialize()
        .collect::<Result<Vec<_>, _>>()
        .map_err(FanduelDataError::Csv)
}

pub(crate) fn parse_csv(
    season_week: SeasonWeek,
    data: &[u8],
) -> Result<Vec<SalariedPlayer>, FanduelDataError> {
    parse_rows(data)?
        .into_iter()
        .map(|row| {
            let position = row.position().ok_or_else(|| {
                FanduelDataError::InvalidRow(format!("unknown position {:?}", row.position))
            })?;
            let salary = row
                .salary
                .parse::<u32>()
                .ok()
                .filter(|salary| *salary > 0)
                .ok_or_else(|| {
                    FanduelDataError::InvalidRow(format!("invalid salary {:?}", row.salary))
                })?;
            let game = Game {
                matchup: row.game.parse::<Matchup>()?,
                season_week: season_week.clone(),
            };
            let team = row
                .team
                .parse::<TeamAbbr>()
                .map_err(|error| FanduelDataError::InvalidRow(error.to_string()))?;

            let player = if position == DfsPosition::Dst {
                None
            } else {
                let first_name = row.first_name.trim().to_owned();
                let last_name = row.last_name.trim().to_owned();
                if first_name.is_empty() || last_name.is_empty() {
                    return Err(FanduelDataError::InvalidRow(
                        "player name is missing".to_owned(),
                    ));
                }
                let id = row.id.rsplit('-').next().unwrap_or(&row.id);
                let id = id.parse::<u32>().ok().filter(|id| *id > 0).ok_or_else(|| {
                    FanduelDataError::InvalidRow(format!("invalid FanDuel player id {:?}", row.id))
                })?;
                Some(Player {
                    id,
                    first_name,
                    last_name,
                })
            };

            Ok(SalariedPlayer {
                player,
                game,
                salary,
                position,
                team,
            })
        })
        .collect()
}
