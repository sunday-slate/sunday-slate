use axum::{
    body::Body,
    http::{HeaderValue, Request, Response, StatusCode, header},
};
use rust_embed::RustEmbed;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    convert::Infallible,
    sync::{LazyLock, Mutex},
    task::{Context, Poll},
    time::{Duration, Instant},
};
use tower::Service;

#[derive(RustEmbed, Clone)]
#[folder = "$CARGO_MANIFEST_DIR/assets"]
pub(crate) struct Assets;

const IMMUTABLE: &str = "public, max-age=31536000, immutable";
const NO_CACHE: &str = "no-cache";

/// How often a memoized digest re-probes the content fingerprints it
/// depends on. Only dev files change; release digests are build-frozen.
const PROBE_TTL: Duration = Duration::from_secs(1);

/// Embedded referencers whose internal asset references get a `?v=` stamp:
/// the referencer's url path plus the (substring in its bytes, leaf url
/// path) pairs to rewrite. A missing substring is served unstamped. Leaves
/// are unstamped, so the dependency order is always leaf first and there
/// are no cycles.
const STAMPS: &[(&str, &[(&str, &str)])] = &[
    (
        "/static/css/app.css",
        &[
            (
                "/static/vendor/fonts/geist.woff2",
                "/static/vendor/fonts/geist.woff2",
            ),
            (
                "/static/vendor/fonts/geist-mono.woff2",
                "/static/vendor/fonts/geist-mono.woff2",
            ),
        ],
    ),
    (
        "/static/vendor/phosphor/regular.css",
        &[("./Phosphor.woff2", "/static/vendor/phosphor/Phosphor.woff2")],
    ),
    (
        "/static/vendor/phosphor/fill.css",
        &[(
            "./Phosphor-Fill.woff2",
            "/static/vendor/phosphor/Phosphor-Fill.woff2",
        )],
    ),
    (
        "/static/manifest.webmanifest",
        &[
            (
                "/static/img/app-icon-192.png",
                "/static/img/app-icon-192.png",
            ),
            (
                "/static/img/app-icon-512.png",
                "/static/img/app-icon-512.png",
            ),
        ],
    ),
];

/// Everything needed to decide whether a memoized digest is still valid.
struct HashMemo {
    /// sha256 of the file's own raw bytes first, then of every leaf it
    /// stamps. Content hashes rather than file mtimes: with
    /// `SOURCE_DATE_EPOCH` set, rust-embed reports the same frozen mtime
    /// for every file, so only content change can be detected.
    fingerprints: Vec<[u8; 32]>,
    /// Bound on how often the fingerprint is re-probed (`Assets::get` reads
    /// whole files in dev); between probes the stored digest is returned as-is.
    probed: Instant,
    /// sha256 of the bytes actually served (stamped for referencers).
    digest: [u8; 32],
}

static HASHES: LazyLock<Mutex<HashMap<String, HashMemo>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

#[derive(Clone)]
pub struct StaticAssetService;

impl Service<Request<Body>> for StaticAssetService {
    type Response = Response<Body>;
    type Error = Infallible;
    type Future = std::future::Ready<Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: Request<Body>) -> Self::Future {
        let req_path = req.uri().path().trim_start_matches('/');
        let url_path = format!("/static/{req_path}");

        let Some(data) = served_bytes(&url_path) else {
            let resp = Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(Body::from("not found"))
                .unwrap();
            return std::future::ready(Ok(resp));
        };

        let mime = mime_guess::from_path(req_path).first_or_octet_stream();

        // Dev only: the file can vanish between the read above and the memo
        // probe; hashing the bytes in hand keeps the ETag honest.
        let digest = served_digest(&url_path).unwrap_or_else(|| Sha256::digest(&data).into());
        let etag = format!("\"{}\"", hex(&digest));
        let cache_control = if url_has_current_version(&req, &digest) {
            IMMUTABLE
        } else {
            NO_CACHE
        };

        if let Some(if_none_match) = req.headers().get(header::IF_NONE_MATCH)
            && let Ok(val) = if_none_match.to_str()
            && val == etag
        {
            let resp = Response::builder()
                .status(StatusCode::NOT_MODIFIED)
                .header(
                    header::CACHE_CONTROL,
                    HeaderValue::from_str(cache_control).unwrap(),
                )
                .body(Body::empty())
                .unwrap();
            return std::future::ready(Ok(resp));
        }

        let resp = Response::builder()
            .status(StatusCode::OK)
            .header(
                header::CONTENT_TYPE,
                HeaderValue::from_str(mime.as_ref()).unwrap(),
            )
            .header(header::ETAG, HeaderValue::from_str(&etag).unwrap())
            .header(
                header::CACHE_CONTROL,
                HeaderValue::from_str(cache_control).unwrap(),
            )
            .body(Body::from(data))
            .unwrap();

        std::future::ready(Ok(resp))
    }
}

/// "/static/x" -> "/static/x?v=<first 16 hex chars of sha256>", or url_path
/// unchanged when not a /static path or when the asset does not exist.
pub fn versioned(url_path: &str) -> String {
    if !url_path.starts_with("/static/") {
        return url_path.to_string();
    }
    match served_digest(url_path) {
        Some(digest) => format!("{url_path}?v={}", hex16(&digest)),
        None => url_path.to_string(), // missing asset 404s loudly at request time
    }
}

fn fs_path(url_path: &str) -> String {
    format!("static/{}", &url_path["/static/".len()..])
}

/// The bytes url_path serves: stamped when listed in `STAMPS`.
fn served_bytes(url_path: &str) -> Option<Vec<u8>> {
    let file = Assets::get(&fs_path(url_path))?;
    let Some((_, pairs)) = STAMPS.iter().find(|(path, _)| *path == url_path) else {
        return Some(file.data.into_owned());
    };

    let mut text = match String::from_utf8(file.data.into_owned()) {
        Ok(text) => text,
        Err(e) => return Some(e.into_bytes()),
    };
    for (haystack, leaf) in *pairs {
        let Some(version) = served_digest(leaf).map(|digest| hex16(&digest)) else {
            continue;
        };
        text = text.replace(haystack, &format!("{haystack}?v={version}"));
    }
    Some(text.into_bytes())
}

/// sha256 of the bytes url_path serves (stamped when it appears in
/// `STAMPS`), memoized against the content fingerprints it depends on;
/// probes are throttled to one per `PROBE_TTL`, so dev hot-reload works
/// without a full read per render.
fn served_digest(url_path: &str) -> Option<[u8; 32]> {
    {
        let memo = HASHES.lock().expect("hash memo");
        if let Some(entry) = memo.get(url_path)
            && entry.probed.elapsed() < PROBE_TTL
        {
            return Some(entry.digest);
        }
    }

    let fingerprints = fingerprints(url_path)?;

    {
        let mut memo = HASHES.lock().expect("hash memo");
        if let Some(entry) = memo.get_mut(url_path)
            && entry.fingerprints == fingerprints
        {
            entry.probed = Instant::now();
            return Some(entry.digest);
        }
    }

    let data = served_bytes(url_path)?;
    let digest: [u8; 32] = Sha256::digest(&data).into();
    HASHES.lock().expect("hash memo").insert(
        url_path.to_string(),
        HashMemo {
            fingerprints,
            probed: Instant::now(),
            digest,
        },
    );
    Some(digest)
}

/// The content hashes this url_path's served bytes depend on: its own raw
/// bytes first, then every leaf it stamps. A vanished leaf hashes as zero
/// and serves unstamped.
fn fingerprints(url_path: &str) -> Option<Vec<[u8; 32]>> {
    let mut fingerprints = vec![raw_hash(url_path)?];
    if let Some((_, pairs)) = STAMPS.iter().find(|(path, _)| *path == url_path) {
        for (_, leaf) in *pairs {
            fingerprints.push(raw_hash(leaf).unwrap_or([0u8; 32]));
        }
    }
    Some(fingerprints)
}

/// sha256 of the raw embedded bytes of one url path.
fn raw_hash(url_path: &str) -> Option<[u8; 32]> {
    let file = Assets::get(&fs_path(url_path))?;
    let digest: [u8; 32] = Sha256::digest(&*file.data).into();
    Some(digest)
}

/// True when the request URL pins the digest's version, meaning the client
/// got it from this binary's own template renders.
fn url_has_current_version(req: &Request<Body>, digest: &[u8; 32]) -> bool {
    req.uri().query().is_some_and(|query| {
        query
            .split('&')
            .any(|kv| kv == format!("v={}", hex16(digest)))
    })
}

fn hex16(digest: &[u8; 32]) -> String {
    hex(&digest[..8])
}

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut hex, byte| {
            use std::fmt::Write;
            let _ = write!(hex, "{byte:02x}");
            hex
        })
}
