// ── Free helpers ──

use std::str::FromStr;
use std::time::Duration;

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::{Connection, SqlitePool};

use crate::mail::Mailer;
use axum_test::multipart::{MultipartForm, Part};
use image::{DynamicImage, ImageFormat, RgbImage};
use std::io::Cursor;

/// A `Mailer` whose `send` always fails (invalid From address).
pub fn failing_mailer() -> Mailer {
    let smtp = crate::mail::transport::SmtpConfig {
        host: "localhost".to_string(),
        port: 587,
        username: "user".to_string(),
        password: utils::Secret::new("pass"),
    };
    let transport = crate::mail::transport::build_transport(&smtp).expect("build smtp transport");
    Mailer::Smtp {
        transport,
        default_from: "not-a-valid-from".to_string(),
    }
}

/// A value as it appears in rendered HTML.
///
/// Templates escape what they interpolate, so asserting a raw string appears in
/// a page body is only right until the value contains a character that escapes.
/// Faker names do: `CompanyName()` produces "O'Conner, Kertzmann and Osinski",
/// and askama renders the apostrophe as `&#x27;` — which made two tests fail
/// intermittently, on nothing but the luck of the generated name.
///
/// Uses askama's own escaper rather than a hand-rolled one, so the expectation
/// cannot drift from the renderer.
pub fn as_rendered(value: &str) -> String {
    use askama::filters::{Html, escape};
    escape(value, Html)
        .expect("escape is infallible")
        .to_string()
}

/// Return the rendered list item containing `needle`.
pub fn list_item_containing<'a>(html: &'a str, needle: &str) -> &'a str {
    let marker = html
        .find(needle)
        .unwrap_or_else(|| panic!("needle not found in rendered HTML: {needle}"));
    let start = html[..marker]
        .rfind("<li")
        .unwrap_or_else(|| panic!("list item not found for: {needle}"));
    let end = marker
        + html[marker..]
            .find("</li>")
            .map(|offset| offset + "</li>".len())
            .unwrap_or_else(|| panic!("list item does not close for: {needle}"));
    &html[start..end]
}

/// Build a URL-encoded form body from key-value pairs.
/// Percent-encodes keys and values per RFC 3986.
pub fn form_body(pairs: &[(&str, &str)]) -> String {
    url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(pairs.iter().copied())
        .finish()
}

/// A migrated, shared in-memory SQLite pool (one connection, so every query
/// sees the same database). Mirrors the pragmas used by `db.rs`.
pub(crate) async fn in_memory_pool() -> SqlitePool {
    let options = SqliteConnectOptions::from_str("sqlite::memory:")
        .expect("parse in-memory sqlite url")
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .foreign_keys(true)
        .busy_timeout(Duration::from_secs(10));

    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .expect("connect in-memory db");

    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .expect("run migrations");

    pool
}

type SqliteFileSet = Vec<(std::path::PathBuf, Option<Vec<u8>>)>;

fn source_file_sets_match(left: &SqliteFileSet, right: &SqliteFileSet) -> bool {
    left == right
}

fn read_sqlite_file_set(database_path: &std::path::Path) -> std::io::Result<SqliteFileSet> {
    [
        database_path.to_path_buf(),
        database_path.with_extension("db-wal"),
        database_path.with_extension("db-journal"),
        database_path.with_extension("db-shm"),
    ]
    .into_iter()
    .map(|path| {
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        };
        Ok((path, bytes))
    })
    .collect()
}

pub(crate) async fn snapshot_sqlite_database(
    source_path: &std::path::Path,
    snapshot_path: &std::path::Path,
) -> Result<SqliteFileSet, String> {
    const MAX_ATTEMPTS: usize = 3;
    let source_paths = read_sqlite_file_set(source_path).map_err(|error| error.to_string())?;

    for _ in 0..MAX_ATTEMPTS {
        let before = read_sqlite_file_set(source_path).map_err(|error| error.to_string())?;
        let staging = tempfile::tempdir().map_err(|error| error.to_string())?;
        let staged_database = staging.path().join(
            source_path
                .file_name()
                .ok_or("source database path has no name")?,
        );

        let source_shm = source_path.with_extension("db-shm");
        for (source_file, contents) in &before {
            if source_file != &source_shm
                && let Some(contents) = contents
            {
                let staged_file = staging.path().join(source_file.file_name().unwrap());
                std::fs::write(staged_file, contents).map_err(|error| error.to_string())?;
            }
        }

        let after_copy = read_sqlite_file_set(source_path).map_err(|error| error.to_string())?;
        if !source_file_sets_match(&before, &after_copy) {
            continue;
        }

        let staged_options = SqliteConnectOptions::new()
            .filename(&staged_database)
            .create_if_missing(false);
        let mut staged = sqlx::SqliteConnection::connect_with(&staged_options)
            .await
            .map_err(|error| error.to_string())?;
        sqlx::query("VACUUM INTO ?1")
            .bind(snapshot_path.to_string_lossy().as_ref())
            .execute(&mut staged)
            .await
            .map_err(|error| error.to_string())?;
        drop(staged);

        let after_snapshot =
            read_sqlite_file_set(source_path).map_err(|error| error.to_string())?;
        if source_file_sets_match(&before, &after_snapshot) {
            return Ok(before);
        }
        std::fs::remove_file(snapshot_path).map_err(|error| error.to_string())?;
    }

    let current = read_sqlite_file_set(source_path).map_err(|error| error.to_string())?;
    let message = if source_file_sets_match(&source_paths, &current) {
        "could not obtain a stable SQLite file set".to_owned()
    } else {
        "SQLite source files changed during all snapshot attempts".to_owned()
    };
    Err(message)
}

pub(crate) struct NflverseCacheSnapshot {
    _directory: tempfile::TempDir,
    pub database_path: std::path::PathBuf,
    source_files: Vec<(std::path::PathBuf, Option<Vec<u8>>)>,
}

impl NflverseCacheSnapshot {
    pub(crate) async fn new() -> Option<Self> {
        let source = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../storage/nflverse-cache.db");
        if !source.exists() {
            return None;
        }

        let directory = tempfile::tempdir().expect("nflverse snapshot tempdir");
        let database_path = directory.path().join("nflverse-cache.db");
        let source_files = snapshot_sqlite_database(&source, &database_path)
            .await
            .unwrap_or_else(|error| {
                panic!("snapshot nflverse cache without source writes: {error}")
            });

        Some(Self {
            _directory: directory,
            database_path,
            source_files,
        })
    }

    pub(crate) fn assert_source_unchanged(&self) {
        for (path, before) in &self.source_files {
            let after = std::fs::read(path).ok();
            assert_eq!(after, *before, "source cache file changed: {path:?}");
        }
    }
}

/// A solid PNG of the given size.
pub fn png_bytes(w: u32, h: u32) -> Vec<u8> {
    let img = DynamicImage::ImageRgb8(RgbImage::new(w, h));
    let mut buf = Cursor::new(Vec::new());
    img.write_to(&mut buf, ImageFormat::Png).expect("encode");
    buf.into_inner()
}

/// A multipart form with `bytes` as the `logo` file part.
pub fn logo_form(bytes: Vec<u8>) -> MultipartForm {
    MultipartForm::new().add_part(
        "logo",
        Part::bytes(bytes)
            .file_name("logo.png")
            .mime_type("image/png"),
    )
}

/// The value of the last `name="..."` attribute before the first `marker`.
/// The marker pins the lookup to one element, so unrelated elements added
/// elsewhere on the page cannot change which attribute is read.
pub fn attr_before(html: &str, name: &str, marker: &str) -> String {
    let end = html
        .find(marker)
        .unwrap_or_else(|| panic!("missing marker {marker} in {html}"));
    let attr = format!("{name}=\"");
    let start = html[..end]
        .rfind(&attr)
        .unwrap_or_else(|| panic!("missing {name} attribute before {marker} in {html}"));
    let value = &html[start + attr.len()..];
    value[..value.find('"').expect("closing attribute quote")].to_string()
}

/// The `href` of the last link before `marker`.
pub fn href_before(html: &str, marker: &str) -> String {
    attr_before(html, "href", marker)
}

/// The `token` query value of the first invite accept link in `html`.
pub fn invite_token_in(html: &str) -> String {
    const PREFIX: &str = "/invite?token=";
    let start = html
        .find(PREFIX)
        .unwrap_or_else(|| panic!("missing accept link in {html}"))
        + PREFIX.len();
    let rest = &html[start..];
    rest[..rest
        .find(['"', '&', '<'])
        .expect("accept link is delimited")]
        .to_string()
}

#[cfg(test)]
mod snapshot_tests {
    use std::str::FromStr;

    use sqlx::{Connection, SqliteConnection, sqlite::SqliteConnectOptions};

    use super::{SqliteFileSet, snapshot_sqlite_database, source_file_sets_match};

    #[test]
    fn changed_source_file_sets_are_rejected() {
        let before: SqliteFileSet = vec![("cache.db-wal".into(), Some(vec![1, 2]))];
        let changed: SqliteFileSet = vec![("cache.db-wal".into(), Some(vec![1, 3]))];
        assert!(!source_file_sets_match(&before, &changed));
    }

    #[tokio::test]
    async fn sqlite_snapshot_includes_wal_commits_without_changing_source_files() {
        let temp = tempfile::tempdir().unwrap();
        let source_path = temp.path().join("source.db");
        let snapshot_path = temp.path().join("snapshot.db");
        let mut source = SqliteConnection::connect_with(
            &SqliteConnectOptions::from_str(&format!("sqlite://{}", source_path.display()))
                .unwrap()
                .create_if_missing(true)
                .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal),
        )
        .await
        .unwrap();
        sqlx::query("CREATE TABLE snapshot_test (value TEXT NOT NULL)")
            .execute(&mut source)
            .await
            .unwrap();
        sqlx::query("INSERT INTO snapshot_test VALUES ('committed-in-wal')")
            .execute(&mut source)
            .await
            .unwrap();

        let source_files = [
            source_path.clone(),
            source_path.with_extension("db-wal"),
            source_path.with_extension("db-shm"),
        ];
        let source_bytes: Vec<_> = source_files
            .iter()
            .filter_map(|path| std::fs::read(path).ok().map(|bytes| (path.clone(), bytes)))
            .collect();
        assert!(source_path.with_extension("db-wal").exists());

        snapshot_sqlite_database(&source_path, &snapshot_path)
            .await
            .unwrap();

        let copied: String = sqlx::query_scalar("SELECT value FROM snapshot_test")
            .fetch_one(
                &mut SqliteConnection::connect_with(
                    &SqliteConnectOptions::from_str(&format!(
                        "sqlite://{}",
                        snapshot_path.display()
                    ))
                    .unwrap(),
                )
                .await
                .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(copied, "committed-in-wal");
        for (path, before) in source_bytes {
            assert_eq!(
                std::fs::read(&path).unwrap(),
                before,
                "source changed: {path:?}"
            );
        }
    }
}
