#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CsvDiagnostic {
    pub message: String,
    pub row_number: Option<u64>,
    pub field: Option<String>,
}
