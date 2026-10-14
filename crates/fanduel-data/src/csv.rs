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

/// Parse typed source rows without persisting them, allowing validation before import.
pub fn parse_csv(
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

#[cfg(test)]
mod tests {
    use super::parse_csv;
    use nfl_model::{DfsPosition, SeasonWeek, TeamAbbr};

    #[test]
    fn parses_fanduel_export_players_games_and_defense() {
        let salaries = parse_csv(
            SeasonWeek::reg(1),
            include_bytes!("../tests/fixtures/fanduel-export.csv"),
        )
        .unwrap();

        assert_eq!(salaries.len(), 4);
        assert_eq!(salaries[0].player.as_ref().unwrap().id, 62239);
        assert_eq!(salaries[0].player.as_ref().unwrap().first_name, "Josh");
        assert_eq!(salaries[0].position, DfsPosition::Qb);
        assert_eq!(salaries[0].team, TeamAbbr::BUF);
        assert_eq!(salaries[0].game.matchup.away, TeamAbbr::BUF);
        assert_eq!(salaries[0].game.season_week, SeasonWeek::reg(1));

        let defense = &salaries[1];
        assert!(defense.player.is_none());
        assert_eq!(defense.position, DfsPosition::Dst);
        assert_eq!(defense.team, TeamAbbr::NYG);

        let jaguar = salaries
            .iter()
            .find(|row| row.team == TeamAbbr::JAX)
            .unwrap();
        assert_eq!(jaguar.game.matchup.away, TeamAbbr::JAX);
        let ram = salaries
            .iter()
            .find(|row| row.team == TeamAbbr::LA)
            .unwrap();
        assert_eq!(ram.game.matchup.away, TeamAbbr::LA);
    }

    #[test]
    fn rejects_an_unrecognized_position_in_the_export() {
        let csv = include_str!("../tests/fixtures/fanduel-export.csv").replacen(",QB,", ",CPT,", 1);
        assert!(matches!(
            parse_csv(SeasonWeek::reg(1), csv.as_bytes()),
            Err(crate::FanduelDataError::InvalidRow(_))
        ));
    }
}
