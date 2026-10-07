mod config;
mod csv;
mod error;
mod model;
mod store;

pub use config::FanduelDataConfig;
pub use csv::interpret;
pub use error::{CsvDiagnostic, FanduelDataError};
pub use model::{Interpretation, InterpretedRow, Matchup, RawSalaryRow, RowDiagnostic};
pub use store::{ReceivedUpload, SourceRow, Upload, UploadMetadata, UploadState};

#[derive(Clone)]
pub struct FanduelData {
    store: store::Store,
}

impl FanduelData {
    pub async fn connect(config: FanduelDataConfig) -> Result<Self, FanduelDataError> {
        Ok(Self {
            store: store::Store::connect(&config.database_url).await?,
        })
    }

    pub async fn in_memory() -> Result<Self, FanduelDataError> {
        Ok(Self {
            store: store::Store::in_memory().await?,
        })
    }

    pub async fn receive(
        &self,
        bytes: &[u8],
        filename: Option<&str>,
    ) -> Result<ReceivedUpload, FanduelDataError> {
        self.store.receive(bytes, filename).await
    }

    pub async fn upload(&self, id: i64) -> Result<Option<Upload>, FanduelDataError> {
        self.store.upload(id).await
    }

    pub async fn source_rows(&self, upload_id: i64) -> Result<Vec<SourceRow>, FanduelDataError> {
        self.store.source_rows(upload_id).await
    }

    pub async fn recent_uploads(
        &self,
        limit: u32,
    ) -> Result<Vec<UploadMetadata>, FanduelDataError> {
        self.store.recent_uploads(limit).await
    }
}
