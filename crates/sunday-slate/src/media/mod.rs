use std::{io, path::Path};

use sqlx::SqliteConnection;
use uuid::Uuid;

use crate::AppError;

pub mod model;
pub mod process;
pub mod store;

pub use model::{Media, MediaId};

pub(crate) async fn write_file_atomic(dir: &Path, file_name: &str, bytes: &[u8]) -> io::Result<()> {
    let final_path = dir.join(file_name);
    let tmp_path = dir.join(format!("{file_name}.tmp"));
    if let Some(parent) = final_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    let result = async {
        tokio::fs::write(&tmp_path, bytes).await?;
        tokio::fs::rename(&tmp_path, &final_path).await
    }
    .await;

    if result.is_err() {
        let _ = tokio::fs::remove_file(&tmp_path).await;
    }
    result
}

/// `file_name` is the path relative to `media_dir`. `None` picks a uuid name.
/// When `existing` is `Some`, the row's current path is kept.
pub async fn save_png(
    conn: &mut SqliteConnection,
    media_dir: &Path,
    existing: Option<MediaId>,
    file_name: Option<&str>,
    png: &[u8],
    width: u32,
    height: u32,
) -> Result<MediaId, AppError> {
    let byte_size = png.len() as i64;
    let (id, file_name) = match existing {
        Some(id) => {
            let current = store::get(&mut *conn, id)
                .await?
                .expect("media foreign key references an existing row");
            store::update_bytes(
                &mut *conn,
                id,
                byte_size,
                i64::from(width),
                i64::from(height),
            )
            .await?;
            (id, current.path)
        }
        None => {
            let file_name = match file_name {
                Some(name) => name.to_owned(),
                None => format!("{}.png", Uuid::new_v4()),
            };
            let id = store::insert(
                &mut *conn,
                &file_name,
                "image/png",
                byte_size,
                i64::from(width),
                i64::from(height),
            )
            .await?;
            (id, file_name)
        }
    };
    write_file_atomic(media_dir, &file_name, png)
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::store;

    #[sqlx::test]
    async fn save_png_uses_the_given_file_name(pool: sqlx::SqlitePool) {
        let dir = tempfile::tempdir().expect("temp media dir");
        let mut conn = pool.acquire().await.expect("conn");
        let id = save_png(
            &mut conn,
            dir.path(),
            None,
            Some("headshots/00-TEST.png"),
            b"png",
            1,
            1,
        )
        .await
        .expect("save");
        let media = store::get(&mut *conn, id).await.expect("get").expect("row");
        assert_eq!(media.path, "headshots/00-TEST.png");
        assert!(dir.path().join("headshots/00-TEST.png").is_file());
    }

    #[sqlx::test]
    async fn save_png_without_a_name_uses_a_uuid(pool: sqlx::SqlitePool) {
        let dir = tempfile::tempdir().expect("temp media dir");
        let mut conn = pool.acquire().await.expect("conn");
        let id = save_png(&mut conn, dir.path(), None, None, b"png", 1, 1)
            .await
            .expect("save");
        let media = store::get(&mut *conn, id).await.expect("get").expect("row");
        assert!(media.path.ends_with(".png"));
        assert!(!media.path.contains('/'));
        assert!(dir.path().join(&media.path).is_file());
    }
}
