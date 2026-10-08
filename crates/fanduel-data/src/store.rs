use std::{path::Path, str::FromStr};

use nfl_model::{DfsPosition, TeamAbbr};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{
    ConnectOptions, SqlitePool,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous},
};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use crate::{
    CsvDiagnostic, FanduelDataError, Interpretation, InterpretedRow, Matchup, RawSalaryRow,
    RowDiagnostic,
};

#[derive(Clone)]
pub(crate) struct Store {
    pool: SqlitePool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UploadState {
    Received,
    Parsed,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadMetadata {
    pub id: i64,
    pub filename: Option<String>,
    pub received_at: OffsetDateTime,
    pub content_hash: String,
    pub state: UploadState,
    pub diagnostic: Option<CsvDiagnostic>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Upload {
    pub metadata: UploadMetadata,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SourceRow {
    pub id: i64,
    pub upload_id: i64,
    pub interpreted: InterpretedRow,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ReceivedUpload {
    pub upload_id: i64,
    pub interpretation: Interpretation,
}

#[derive(Serialize, Deserialize)]
struct StoredRow {
    row_number: u64,
    raw: RawSalaryRow,
    salary: Option<i64>,
    team: TeamAbbr,
    position: Option<String>,
    matchup: Option<Matchup>,
    diagnostics: Vec<RowDiagnostic>,
}

impl From<&InterpretedRow> for StoredRow {
    fn from(row: &InterpretedRow) -> Self {
        Self {
            row_number: row.row_number,
            raw: row.raw.clone(),
            salary: row.salary,
            team: row.team.clone(),
            position: row.position.map(|position| position.label().to_owned()),
            matchup: row.matchup.clone(),
            diagnostics: row.diagnostics.clone(),
        }
    }
}

impl TryFrom<StoredRow> for InterpretedRow {
    type Error = FanduelDataError;

    fn try_from(row: StoredRow) -> Result<Self, Self::Error> {
        let position = row
            .position
            .map(|position| match position.as_str() {
                "QB" => Ok(DfsPosition::Qb),
                "RB" => Ok(DfsPosition::Rb),
                "WR" => Ok(DfsPosition::Wr),
                "TE" => Ok(DfsPosition::Te),
                "D/ST" => Ok(DfsPosition::Dst),
                _ => Err(FanduelDataError::Position(position)),
            })
            .transpose()?;
        Ok(Self {
            row_number: row.row_number,
            raw: row.raw,
            team: row.team,
            salary: row.salary,
            position,
            matchup: row.matchup,
            diagnostics: row.diagnostics,
        })
    }
}

fn pool_options(options: &SqliteConnectOptions, pool: SqlitePoolOptions) -> SqlitePoolOptions {
    let filename = options.get_filename();
    // SQLx 0.9 serializes its generated `file:sqlx-in-memory-*` filenames as invalid URLs.
    let is_sqlx_memory = filename
        .to_string_lossy()
        .starts_with("file:sqlx-in-memory-");
    let is_memory = filename == Path::new(":memory:")
        || is_sqlx_memory
        || options
            .to_url_lossy()
            .query_pairs()
            .any(|(key, value)| key == "mode" && value == "memory");
    if is_memory {
        // Retiring the last memory connection destroys both archive data and migrations.
        pool.idle_timeout(None).max_lifetime(None)
    } else {
        pool
    }
}

impl Store {
    pub(crate) async fn connect(database_url: &str) -> Result<Self, FanduelDataError> {
        let options = SqliteConnectOptions::from_str(database_url)?
            .create_if_missing(true)
            .foreign_keys(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Full);
        let pool = pool_options(&options, SqlitePoolOptions::new().max_connections(1))
            .connect_with(options)
            .await?;
        Self::migrate(pool).await
    }

    pub(crate) async fn in_memory() -> Result<Self, FanduelDataError> {
        let options = SqliteConnectOptions::new()
            .filename(":memory:")
            .foreign_keys(true)
            .synchronous(SqliteSynchronous::Full);
        let pool = pool_options(&options, SqlitePoolOptions::new().max_connections(1))
            .connect_with(options)
            .await?;
        Self::migrate(pool).await
    }

    async fn migrate(pool: SqlitePool) -> Result<Self, FanduelDataError> {
        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .map_err(|error| FanduelDataError::Sqlx(sqlx::Error::Migrate(Box::new(error))))?;
        Ok(Self { pool })
    }

    pub(crate) async fn receive(
        &self,
        bytes: &[u8],
        filename: Option<&str>,
    ) -> Result<ReceivedUpload, FanduelDataError> {
        let received_at = OffsetDateTime::now_utc();
        let received_at_text = received_at.format(&Rfc3339)?;
        let content_hash = Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let mut tx = self.pool.begin().await?;
        let result = sqlx::query(
            "INSERT INTO uploads (filename, received_at, content_hash, bytes, state) VALUES (?, ?, ?, ?, 'received')",
        )
        .bind(filename)
        .bind(received_at_text)
        .bind(&content_hash)
        .bind(bytes)
        .execute(&mut *tx)
        .await?;
        let upload_id = result.last_insert_rowid();
        tx.commit().await?;

        let interpretation = crate::interpret(bytes);
        match &interpretation {
            Interpretation::Empty => {
                sqlx::query("UPDATE uploads SET state = 'parsed' WHERE id = ?")
                    .bind(upload_id)
                    .execute(&self.pool)
                    .await?;
            }
            Interpretation::InvalidCsv(diagnostic) => {
                let diagnostic = serde_json::to_string(diagnostic)?;
                sqlx::query("UPDATE uploads SET state = 'failed', diagnostic = ? WHERE id = ?")
                    .bind(diagnostic)
                    .bind(upload_id)
                    .execute(&self.pool)
                    .await?;
            }
            Interpretation::Parsed(rows) => {
                let mut tx = self.pool.begin().await?;
                for row in rows {
                    let stored = serde_json::to_string(&StoredRow::from(row))?;
                    sqlx::query("INSERT INTO source_rows (upload_id, row_number, interpreted) VALUES (?, ?, ?)")
                        .bind(upload_id).bind(row.row_number as i64).bind(stored).execute(&mut *tx).await?;
                }
                sqlx::query("UPDATE uploads SET state = 'parsed' WHERE id = ?")
                    .bind(upload_id)
                    .execute(&mut *tx)
                    .await?;
                tx.commit().await?;
            }
        }
        Ok(ReceivedUpload {
            upload_id,
            interpretation,
        })
    }

    pub(crate) async fn upload(&self, id: i64) -> Result<Option<Upload>, FanduelDataError> {
        let row = sqlx::query_as::<_, (i64, Option<String>, String, String, String, Option<String>, Vec<u8>)>(
            "SELECT id, filename, received_at, content_hash, state, diagnostic, bytes FROM uploads WHERE id = ?",
        ).bind(id).fetch_optional(&self.pool).await?;
        row.map(
            |(id, filename, received_at, content_hash, state, diagnostic, bytes)| {
                Ok(Upload {
                    metadata: UploadMetadata {
                        id,
                        filename,
                        received_at: OffsetDateTime::parse(&received_at, &Rfc3339)?,
                        content_hash,
                        state: parse_state(&state),
                        diagnostic: diagnostic
                            .map(|value| serde_json::from_str(&value))
                            .transpose()?,
                    },
                    bytes,
                })
            },
        )
        .transpose()
    }

    pub(crate) async fn source_rows(
        &self,
        upload_id: i64,
    ) -> Result<Vec<SourceRow>, FanduelDataError> {
        let rows = sqlx::query_as::<_, (i64, String)>(
            "SELECT id, interpreted FROM source_rows WHERE upload_id = ? ORDER BY row_number",
        )
        .bind(upload_id)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|(id, payload)| {
                let stored: StoredRow = serde_json::from_str(&payload)?;
                Ok(SourceRow {
                    id,
                    upload_id,
                    interpreted: stored.try_into()?,
                })
            })
            .collect()
    }

    pub(crate) async fn recent_uploads(
        &self,
        limit: u32,
    ) -> Result<Vec<UploadMetadata>, FanduelDataError> {
        let rows = sqlx::query_as::<_, (i64, Option<String>, String, String, String, Option<String>)>(
            "SELECT id, filename, received_at, content_hash, state, diagnostic FROM uploads ORDER BY id DESC LIMIT ?",
        ).bind(i64::from(limit)).fetch_all(&self.pool).await?;
        rows.into_iter()
            .map(
                |(id, filename, received_at, content_hash, state, diagnostic)| {
                    Ok(UploadMetadata {
                        id,
                        filename,
                        received_at: OffsetDateTime::parse(&received_at, &Rfc3339)?,
                        content_hash,
                        state: parse_state(&state),
                        diagnostic: diagnostic
                            .map(|value| serde_json::from_str(&value))
                            .transpose()?,
                    })
                },
            )
            .collect()
    }
}

fn parse_state(state: &str) -> UploadState {
    match state {
        "parsed" => UploadState::Parsed,
        "failed" => UploadState::Failed,
        _ => UploadState::Received,
    }
}

#[cfg(test)]
mod tests {
    use super::{Store, UploadState, pool_options};
    use crate::Interpretation;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions, SqliteSynchronous};
    use std::{str::FromStr, time::Duration};

    const CSV: &[u8] = include_bytes!("../tests/fixtures/salaries.csv");

    #[tokio::test]
    async fn memory_archives_survive_connection_retirement_policy() {
        let cases = [
            (
                "low-level filename",
                SqliteConnectOptions::new().filename(":memory:"),
                false,
            ),
            (
                "sqlite::memory:",
                SqliteConnectOptions::from_str("sqlite::memory:").unwrap(),
                true,
            ),
            (
                "sqlite://:memory:",
                SqliteConnectOptions::from_str("sqlite://:memory:").unwrap(),
                true,
            ),
            (
                "anonymous memory URL",
                SqliteConnectOptions::from_str("sqlite://?mode=memory").unwrap(),
                true,
            ),
            (
                "named private memory URL",
                SqliteConnectOptions::from_str("sqlite://named-archive?mode=memory&cache=private")
                    .unwrap(),
                true,
            ),
            (
                "named shared memory URL",
                SqliteConnectOptions::from_str("sqlite://named-archive?mode=memory&cache=shared")
                    .unwrap(),
                true,
            ),
        ];

        for (name, options, is_url) in cases {
            let mut options = options
                .foreign_keys(true)
                .synchronous(SqliteSynchronous::Full);
            if is_url {
                options = options.journal_mode(sqlx::sqlite::SqliteJournalMode::Wal);
            }
            let pool = pool_options(
                &options,
                SqlitePoolOptions::new()
                    .max_connections(1)
                    .max_lifetime(Duration::ZERO),
            )
            .connect_with(options)
            .await
            .unwrap_or_else(|error| panic!("{name}: {error}"));
            let store = Store::migrate(pool)
                .await
                .unwrap_or_else(|error| panic!("{name}: {error}"));

            let first = store.receive(CSV, Some("retirement.csv")).await.unwrap();
            assert!(
                matches!(first.interpretation, Interpretation::Parsed(_)),
                "{name}"
            );
            let upload = store.upload(first.upload_id).await.unwrap().unwrap();
            assert_eq!(upload.bytes, CSV, "{name}");
            assert_eq!(
                upload.metadata.filename.as_deref(),
                Some("retirement.csv"),
                "{name}"
            );
            assert_eq!(upload.metadata.state, UploadState::Parsed, "{name}");
            let rows = store.source_rows(first.upload_id).await.unwrap();
            assert_eq!(rows.len(), 2, "{name}");
            assert_eq!(rows[0].interpreted.raw.id, "123506-62239", "{name}");
            assert_eq!(rows[0].interpreted.raw.salary, "8200", "{name}");
            assert_eq!(rows[0].interpreted.salary, Some(8200), "{name}");
            assert_eq!(rows[1].interpreted.raw.id, "119110-12543", "{name}");
            assert_eq!(rows[1].interpreted.raw.salary, "3000", "{name}");
            assert_eq!(rows[1].interpreted.salary, Some(3000), "{name}");

            let second = store.receive(CSV, Some("retirement.csv")).await.unwrap();
            assert_ne!(second.upload_id, first.upload_id, "{name}");
            assert_eq!(
                store.upload(first.upload_id).await.unwrap().unwrap().bytes,
                CSV,
                "{name}"
            );
            store.pool.close().await;
        }
    }
}
