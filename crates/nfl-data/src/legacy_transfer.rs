//! One-time transfer of legacy player links from Sunday Slate's pre-cutover
//! database into the NFL data store.

use std::collections::BTreeMap;
use std::path::Path;
use std::str::FromStr;

use anyhow::{Context, bail};
use sqlx::Connection;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

/// Result of a successful transfer.
#[doc(hidden)]
pub struct LinkMigrationReport {
    pub copied: usize,
    pub already_present: usize,
}

/// Copy legacy player links from a pre-cutover Sunday Slate database into the
/// NFL data store. Source is opened read-only and never modified; the
/// destination is created/migrated if missing. Same-file source/destination is
/// rejected before anything is written, by filesystem identity (device +
/// inode), so aliases like relative vs. absolute spellings, symlinks, and hard
/// links are all caught.
#[doc(hidden)]
pub async fn migrate_salary_links(
    source_url: &str,
    destination_url: &str,
) -> anyhow::Result<LinkMigrationReport> {
    let source_path = sqlite_path(source_url).context("parsing --source as a SQLite URL")?;
    let destination_path =
        sqlite_path(destination_url).context("parsing --destination as a SQLite URL")?;
    if source_path == destination_path {
        bail!("source and destination resolve to the same SQLite file; use a separate destination");
    }
    if let Some(conflict) = same_file(&source_path, &destination_path)? {
        bail!(
            "source and destination are the same file through different paths ({conflict}); \
             use a separate destination"
        );
    }

    // The source must exist: opening read-only without create_if_missing
    // fails instead of creating an empty database.
    let source_options = SqliteConnectOptions::from_str(source_url)
        .context("parsing --source as a SQLite URL")?
        .read_only(true)
        .create_if_missing(false);
    let source = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(source_options)
        .await
        .context("opening the source database (legacy player links are unavailable; use the pre-cutover Sunday Slate backup)")?;

    let rows: Vec<(i64, String, String, String)> = {
        let mut conn = source
            .acquire()
            .await
            .context("connecting to the source database")?;
        let mut tx = conn
            .begin()
            .await
            .context("starting a read transaction on the source")?;

        let columns: Vec<String> =
            sqlx::query_scalar("SELECT name FROM pragma_table_info('nfl_players')")
                .fetch_all(&mut *tx)
                .await
                .context("reading the source table layout")?;
        if !columns.iter().any(|c| c == "fd_player_id") {
            bail!("legacy player links are unavailable; use the pre-cutover Sunday Slate backup");
        }

        let rows = sqlx::query_as::<_, (i64, String, String, String)>(
            r#"SELECT fd_player_id, gsis_player_id, created_at, updated_at
               FROM nfl_players
               WHERE fd_player_id IS NOT NULL
               ORDER BY fd_player_id"#,
        )
        .fetch_all(&mut *tx)
        .await
        .context("reading legacy player links")?;
        tx.commit().await?;
        rows
    };

    let destination = crate::store::Store::open(destination_url)
        .await
        .map_err(|e| anyhow::anyhow!("preparing the destination NFL data store: {e}"))?;

    // Group by FD id: one source row per FD id, or an ambiguous legacy state.
    let mut by_fd: BTreeMap<u32, (String, String, String)> = BTreeMap::new();
    for (fd, gsis, created_at, updated_at) in rows {
        let fd_id = u32::try_from(fd)
            .with_context(|| format!("legacy fd_player_id {fd} is out of range"))?;
        let replaced = by_fd.insert(fd_id, (gsis, created_at, updated_at));
        anyhow::ensure!(
            replaced.is_none(),
            "source holds multiple rows for fd_player_id {fd_id}"
        );
    }

    let mut copied = 0usize;
    let mut already_present = 0usize;

    destination
        .write_tx(async |conn| {
            for (fd_id, (gsis, created_at, updated_at)) in &by_fd {
                let existing: Option<(i64, String)> =
                    sqlx::query_as("SELECT fd_player_id, gsis_player_id FROM player_links WHERE gsis_player_id = ?1")
                        .bind(gsis)
                        .fetch_optional(&mut *conn)
                        .await?;
                if existing == Some((*fd_id as i64, gsis.clone())) {
                    already_present += 1;
                    continue;
                }
                if let Some((existing_fd, _)) = existing {
                    // A different FD id is already linked to this GSIS id.
                    // Ordinary uniqueness rules abort the transfer; no special
                    // conflict review.
                    return Err(crate::salaries::SalaryError::Database(sqlx::Error::Protocol(format!(
                        "GSIS id {gsis} is already linked to fd id {existing_fd}"
                    ))));
                }
                sqlx::query(
                    r#"INSERT INTO player_links (fd_player_id, gsis_player_id, created_at, updated_at)
                       VALUES (?1, ?2, ?3, ?4)"#,
                )
                .bind(i64::from(*fd_id))
                .bind(gsis)
                .bind(created_at)
                .bind(updated_at)
                .execute(&mut *conn)
                .await?;
                copied += 1;
            }
            // Verify every source pair inside the transaction before commit.
            for (fd_id, (gsis, _, _)) in &by_fd {
                let stored: Option<i64> = sqlx::query_scalar("SELECT fd_player_id FROM player_links WHERE gsis_player_id = ?1")
                    .bind(gsis)
                    .fetch_optional(&mut *conn)
                    .await?;
                if stored != Some(i64::from(*fd_id)) {
                    return Err(crate::salaries::SalaryError::Database(sqlx::Error::Protocol(format!(
                        "post-transfer verification failed for fd id {fd_id}"
                    ))));
                }
            }
            Ok(())
        })
        .await
        .map_err(|e| anyhow::anyhow!("transferring player links: {e}"))?;

    Ok(LinkMigrationReport {
        copied,
        already_present,
    })
}

/// True when both paths exist and are hard links or symlinks to the same
/// file. `Err` only when reading identity fails on an existing path; missing
/// paths compare as distinct (the later source open fails on its own).
fn same_file(source: &str, destination: &str) -> anyhow::Result<Option<String>> {
    let source_path = Path::new(source);
    let destination_path = Path::new(destination);
    if !source_path.exists() || !destination_path.exists() {
        return Ok(None);
    }
    let source_meta = source_path
        .metadata()
        .with_context(|| format!("reading file identity of the source path {source:?}"))?;
    let destination_meta = destination_path.metadata().with_context(|| {
        format!("reading file identity of the destination path {destination:?}")
    })?;
    use std::os::unix::fs::MetadataExt;
    if source_meta.dev() == destination_meta.dev() && source_meta.ino() == destination_meta.ino() {
        Ok(Some(format!("{source:?} and {destination:?}")))
    } else {
        Ok(None)
    }
}

/// Resolve a SQLite URL to its file path, using sqlx's own parsing so the
/// result matches the path sqlx actually opens — percent-decoding and query
/// parameters included. In-memory URLs return `":memory:"`.
fn sqlite_path(url: &str) -> anyhow::Result<String> {
    let options = SqliteConnectOptions::from_str(url).context("invalid SQLite URL")?;
    let filename = options.get_filename().to_string_lossy().into_owned();
    // sqlx maps `:memory:` spellings to a unique per-parse sentinel name, so
    // every in-memory database compares as its own; collapse them instead.
    if filename.starts_with("file:sqlx-in-memory-") {
        return Ok(":memory:".to_owned());
    }
    Ok(filename)
}
