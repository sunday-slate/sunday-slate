// ── Free helpers ──

use std::str::FromStr;
use std::time::Duration;

use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};

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
        password: nfl_data::Secret::new("pass"),
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
