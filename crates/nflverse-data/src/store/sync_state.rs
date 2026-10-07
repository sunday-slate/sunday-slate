use sqlx::{SqliteConnection, SqlitePool};
use time::OffsetDateTime;

use crate::error::NflDataError;

/// Identity of one release asset being written; recorded with the data in the
/// same transaction so cache state can never drift from table contents.
pub(crate) struct SyncedAsset<'a> {
    pub release_tag: &'a str,
    pub asset_name: &'a str,
    pub updated_at: &'a str,
}
pub(crate) struct FreshnessRow {
    pub release_tag: String,
    pub assets: u32,
    pub last_synced_at: OffsetDateTime,
}

pub(crate) async fn freshness(pool: &SqlitePool) -> Result<Vec<FreshnessRow>, NflDataError> {
    Ok(sqlx::query_as!(
        FreshnessRow,
        r#"SELECT release_tag AS "release_tag!: String",
                  COUNT(*) AS "assets!: u32",
                  strftime('%Y-%m-%dT%H:%M:%SZ', MAX(synced_at))
                      AS "last_synced_at!: OffsetDateTime"
           FROM sync_state
           GROUP BY release_tag"#
    )
    .fetch_all(pool)
    .await?)
}

pub(crate) async fn is_current(
    pool: &SqlitePool,
    release_tag: &str,
    asset_name: &str,
    updated_at: &str,
) -> Result<bool, NflDataError> {
    let stored = sqlx::query_scalar!(
        r#"SELECT upstream_updated_at FROM sync_state WHERE release_tag = ? AND asset_name = ?"#,
        release_tag,
        asset_name
    )
    .fetch_optional(pool)
    .await?;
    Ok(stored.as_deref() == Some(updated_at))
}

pub(crate) async fn record(
    conn: &mut SqliteConnection,
    asset: &SyncedAsset<'_>,
) -> Result<(), NflDataError> {
    sqlx::query!(
        r#"INSERT INTO sync_state (release_tag, asset_name, upstream_updated_at)
           VALUES (?, ?, ?)
           ON CONFLICT (release_tag, asset_name) DO UPDATE SET
               upstream_updated_at = excluded.upstream_updated_at,
               synced_at = datetime('now', 'subsec')"#,
        asset.release_tag,
        asset.asset_name,
        asset.updated_at
    )
    .execute(conn)
    .await?;
    Ok(())
}
