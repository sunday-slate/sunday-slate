#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CsvDiagnostic {
    pub message: String,
    pub row_number: Option<u64>,
    pub field: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum FanduelDataError {
    #[error("FanDuel source store error: {0}")]
    Sqlx(#[from] sqlx::Error),
    #[error("FanDuel source store serialization error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("FanDuel source store timestamp error: {0}")]
    Timestamp(#[from] time::error::Parse),
    #[error("FanDuel source store timestamp formatting error: {0}")]
    TimestampFormat(#[from] time::error::Format),
    #[error("unsupported stored FanDuel position {0:?}")]
    Position(String),
}
