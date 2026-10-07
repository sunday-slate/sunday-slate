use std::io::Cursor;

use csv::ReaderBuilder;
use nfl_model::TeamAbbr;

use crate::{CsvDiagnostic, Interpretation, InterpretedRow, Matchup, RawSalaryRow, RowDiagnostic};

pub fn interpret(bytes: &[u8]) -> Interpretation {
    let mut reader = ReaderBuilder::new().from_reader(Cursor::new(bytes));
    let headers = match reader.headers() {
        Ok(headers) if headers.is_empty() => return Interpretation::Empty,
        Ok(headers) => headers.clone(),
        Err(error) => return Interpretation::InvalidCsv(csv_diagnostic(error, None)),
    };

    let rows: Result<Vec<RawSalaryRow>, csv::Error> = reader.deserialize().collect();
    let raw_rows = match rows {
        Ok(rows) if rows.is_empty() => return Interpretation::Empty,
        Ok(rows) => rows,
        Err(error) => return Interpretation::InvalidCsv(csv_diagnostic(error, Some(&headers))),
    };

    Interpretation::Parsed(
        raw_rows
            .into_iter()
            .enumerate()
            .map(|(index, raw)| interpret_row(index as u64 + 1, raw))
            .collect(),
    )
}

fn csv_diagnostic(error: csv::Error, headers: Option<&csv::StringRecord>) -> CsvDiagnostic {
    let field = match error.kind() {
        csv::ErrorKind::Deserialize { err, .. } => err
            .field()
            .and_then(|index| usize::try_from(index).ok())
            .and_then(|index| headers.and_then(|headers| headers.get(index)))
            .map(str::to_owned),
        _ => None,
    };
    CsvDiagnostic {
        message: error.to_string(),
        row_number: error.position().map(|position| position.record()),
        field,
    }
}

fn interpret_row(row_number: u64, raw: RawSalaryRow) -> InterpretedRow {
    let team = TeamAbbr(normalize_team(&raw.team).to_owned());
    let position = raw.dfs_position();
    let matchup = raw.game.split_once('@').map(|(away, home)| Matchup {
        away: TeamAbbr(normalize_team(away).to_owned()),
        home: TeamAbbr(normalize_team(home).to_owned()),
    });
    let salary = raw.parsed_salary();
    let mut diagnostics = Vec::new();
    if position.is_none() {
        diagnostics.push(RowDiagnostic {
            row_number,
            field: "Position".to_owned(),
            message: format!(
                "Unrecognized FanDuel position {:?} for {}",
                raw.position,
                raw.name()
            ),
        });
    }
    if salary.is_none() {
        diagnostics.push(RowDiagnostic {
            row_number,
            field: "Salary".to_owned(),
            message: format!(
                "Could not interpret FanDuel salary {:?} for {}",
                raw.salary,
                raw.name()
            ),
        });
    }
    if matchup.is_none() {
        diagnostics.push(RowDiagnostic {
            row_number,
            field: "Game".to_owned(),
            message: format!(
                "Could not interpret FanDuel matchup {:?} for {}",
                raw.game,
                raw.name()
            ),
        });
    }
    InterpretedRow {
        row_number,
        raw,
        team,
        position,
        matchup,
        salary,
        diagnostics,
    }
}

fn normalize_team(code: &str) -> &str {
    match code {
        "JAC" => "JAX",
        "LAR" => "LA",
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Interpretation, interpret};

    const HEADER: &str = "Id,Position,First Name,Nickname,Last Name,FPPG,Played,Salary,Game,Team,Opponent,Injury Indicator,Injury Details";

    #[test]
    fn interprets_player_and_defense_rows() {
        let bytes = include_bytes!("../tests/fixtures/fanduel-export.csv");
        let Interpretation::Parsed(rows) = interpret(bytes) else {
            panic!("expected parsed rows");
        };
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[0].raw.fd_player_id(), "62239");
        assert_eq!(rows[0].raw.name(), "Josh Allen");
        assert_eq!(rows[0].raw.nickname, "Josh Allen");
        assert_eq!(rows[0].team.0, "BUF");
        assert_eq!(
            rows[0].position.map(|position| position.label()),
            Some("QB")
        );
        let player_matchup = rows[0].matchup.as_ref().unwrap();
        assert_eq!(player_matchup.away.0, "BUF");
        assert_eq!(player_matchup.home.0, "NYJ");
        assert_eq!(rows[0].raw.salary, "8200");
        assert_eq!(rows[0].salary, Some(8200));
        assert_eq!(rows[1].raw.fd_player_id(), "12543");
        assert_eq!(rows[1].raw.name(), "New York Giants");
        assert_eq!(rows[1].raw.nickname, "New York Giants");
        assert_eq!(
            rows[1].position.map(|position| position.label()),
            Some("D/ST")
        );
        assert_eq!(rows[1].raw.salary, "3000");
        assert_eq!(rows[1].salary, Some(3000));
        assert_eq!(rows[2].raw.fd_player_id(), "80796");
        assert_eq!(rows[2].team.0, "JAX");
        assert_eq!(rows[2].raw.injury_indicator, "Q");
        assert_eq!(rows[2].raw.injury_details, "Hamstring");
        assert_eq!(rows[3].raw.fd_player_id(), "99999");
        assert_eq!(rows[3].team.0, "LA");
        assert_eq!(rows[3].raw.fppg, "");
        assert_eq!(rows[3].raw.played, "0");
    }

    #[test]
    fn preserves_bare_ids_and_team_aliases() {
        let bytes = format!("{HEADER}\n62239,QB,Josh,,Allen,25.5,17,8200,JAC@LAR,JAC,LAR,,\n");
        let Interpretation::Parsed(rows) = interpret(bytes.as_bytes()) else {
            panic!("expected parsed rows");
        };
        assert_eq!(rows[0].raw.fd_player_id(), "62239");
        assert_eq!(rows[0].team.0, "JAX");
        let matchup = rows[0].matchup.as_ref().unwrap();
        assert_eq!(matchup.away.0, "JAX");
        assert_eq!(matchup.home.0, "LA");
    }

    #[test]
    fn recognizes_position_aliases_case_insensitively() {
        let positions = ["QB", "rb", "WR", "te", "DST", "DEF", "d"];
        let expected = ["QB", "RB", "WR", "TE", "D/ST", "D/ST", "D/ST"];
        for (position, expected) in positions.into_iter().zip(expected) {
            let bytes = format!("{HEADER}\n1,{position},A,,B,0,0,1,BUF@NYJ,BUF,NYJ,,\n");
            let Interpretation::Parsed(rows) = interpret(bytes.as_bytes()) else {
                panic!("expected parsed row for {position}");
            };
            assert_eq!(rows[0].position.map(|value| value.label()), Some(expected));
        }
    }

    #[test]
    fn keeps_row_diagnostics_for_unknown_position_and_missing_at() {
        let bytes = format!("{HEADER}\n1,CPT,A,,B,0,0,1,BUF-NYJ,BUF,NYJ,,\n");
        let Interpretation::Parsed(rows) = interpret(bytes.as_bytes()) else {
            panic!("expected parsed row");
        };
        assert_eq!(rows[0].row_number, 1);
        assert_eq!(rows[0].position, None);
        assert_eq!(rows[0].matchup, None);
        assert_eq!(rows[0].diagnostics.len(), 2);
        assert_eq!(rows[0].diagnostics[0].field, "Position");
        assert_eq!(rows[0].diagnostics[1].field, "Game");
    }

    #[test]
    fn reports_unparseable_salary_with_row_diagnostic() {
        let bytes = format!("{HEADER}\n1,QB,A,,B,0,0,not-a-number,BUF@NYJ,BUF,NYJ,,\n");
        let Interpretation::Parsed(rows) = interpret(bytes.as_bytes()) else {
            panic!("expected parsed rows");
        };
        assert_eq!(rows[0].salary, None);
        assert_eq!(rows[0].raw.salary, "not-a-number");
        assert_eq!(rows[0].diagnostics.len(), 1);
        let diagnostic = &rows[0].diagnostics[0];
        assert_eq!(diagnostic.row_number, 1);
        assert_eq!(diagnostic.field, "Salary");
        assert!(diagnostic.message.contains("not-a-number"));
    }

    #[test]
    fn empty_and_header_only_uploads_are_empty() {
        assert!(matches!(interpret(b""), Interpretation::Empty));
        assert!(matches!(
            interpret(format!("{HEADER}\n").as_bytes()),
            Interpretation::Empty
        ));
    }

    #[test]
    fn rejects_records_with_inconsistent_field_counts() {
        let bytes = format!(
            "{HEADER}\n1,QB,A,,B,0,0,1,BUF@NYJ,BUF,NYJ,,\n2,RB,C,,D,0,0,1,BUF@NYJ,BUF,NYJ,,,unexpected\n"
        );
        assert!(matches!(
            interpret(bytes.as_bytes()),
            Interpretation::InvalidCsv(_)
        ));
    }

    #[test]
    fn extra_columns_and_multiline_names_preserve_records() {
        let bytes = format!(
            "{HEADER},Future Column\n1,QB,Josh,,\"Allen\nJr.\",25.5,17,8200,BUF@NYJ,BUF,NYJ,,,preserved\n2,RB,James,,Cook,0,0,6000,BUF@NYJ,BUF,NYJ,,,also-preserved\n"
        );
        let Interpretation::Parsed(rows) = interpret(bytes.as_bytes()) else {
            panic!("expected parsed records");
        };
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].row_number, 1);
        assert_eq!(rows[0].raw.last_name, "Allen\nJr.");
        assert_eq!(rows[1].row_number, 2);
        assert_eq!(rows[1].raw.fd_player_id(), "2");
        assert_eq!(bytes.as_bytes().last(), Some(&b'\n'));
    }
}
