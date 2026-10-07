mod csv;
mod error;
mod model;

pub use csv::interpret;
pub use error::CsvDiagnostic;
pub use model::{Interpretation, InterpretedRow, Matchup, RawSalaryRow, RowDiagnostic};
