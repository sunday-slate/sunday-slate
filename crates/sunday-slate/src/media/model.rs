use time::PrimitiveDateTime;

/// A media row's id as an opaque token: typed so it cannot be confused with
/// other ids, open because the type distinction is the protection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, sqlx::Type)]
#[sqlx(transparent)]
pub struct MediaId(pub i64);

#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct Media {
    pub id: MediaId,
    pub path: String,
    pub content_type: String,
    pub byte_size: i64,
    pub width: i64,
    pub height: i64,
    pub created_at: PrimitiveDateTime,
    pub updated_at: PrimitiveDateTime,
}

impl Media {
    /// Public URL, e.g. `/media/7.png?v=...` — `v` is `updated_at` in epoch
    /// nanos, so an in-place overwrite busts the cache.
    pub fn url(&self) -> String {
        let v = self.updated_at.assume_utc().unix_timestamp_nanos();
        format!("/media/{path}?v={v}", path = self.path)
    }
}

#[cfg(test)]
impl Media {
    pub fn fixture(path: &str, updated_at: PrimitiveDateTime) -> Media {
        Media {
            id: MediaId(7),
            path: path.to_string(),
            content_type: "image/png".to_string(),
            byte_size: 1,
            width: 256,
            height: 256,
            created_at: updated_at,
            updated_at,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    #[test]
    fn url_builds_media_path_with_version() {
        let url = Media::fixture("7.png", datetime!(2026-08-09 12:00:00)).url();
        assert!(url.starts_with("/media/7.png?v="), "url = {url}");
    }

    #[test]
    fn url_changes_when_updated_at_changes() {
        let a = Media::fixture("7.png", datetime!(2026-08-09 12:00:00)).url();
        let b = Media::fixture("7.png", datetime!(2026-08-09 12:00:01)).url();
        assert_ne!(a, b);
    }
}
