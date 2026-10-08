use axum::{
    body::Body,
    extract::Path,
    http::{HeaderMap, HeaderValue, Method, StatusCode, header},
    response::{IntoResponse, Response},
};
use rust_embed::RustEmbed;
use sha2::{Digest, Sha256};

#[derive(RustEmbed)]
#[folder = "src/web/assets/static/"]
struct AdminAssets;

pub(super) async fn serve(
    Path(path): Path<String>,
    method: Method,
    headers: HeaderMap,
) -> Response {
    if path.split('/').any(|part| part == "..") {
        return StatusCode::NOT_FOUND.into_response();
    }
    let Some(file) = AdminAssets::get(&path) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    asset_response(&method, &path, &headers, file.data.as_ref())
}

fn asset_response(method: &Method, path: &str, headers: &HeaderMap, bytes: &[u8]) -> Response {
    let digest = Sha256::digest(bytes);
    let etag = format!("\"{}\"", hex_digest(&digest));
    let mut response = if headers
        .get_all(header::IF_NONE_MATCH)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .any(|value| if_none_match(value, &etag))
    {
        StatusCode::NOT_MODIFIED.into_response()
    } else if method == Method::HEAD {
        let mut response = Response::new(Body::empty());
        response.headers_mut().insert(
            header::CONTENT_LENGTH,
            HeaderValue::from_str(&bytes.len().to_string())
                .expect("asset length is a valid header"),
        );
        response
    } else {
        Response::new(Body::from(bytes.to_vec()))
    };

    if response.status() == StatusCode::OK {
        let content_type = mime_guess::from_path(path)
            .first_or_octet_stream()
            .essence_str()
            .to_owned();
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_str(&content_type)
                .unwrap_or(HeaderValue::from_static("application/octet-stream")),
        );
    }
    response.headers_mut().insert(
        header::ETAG,
        HeaderValue::from_str(&etag).expect("quoted hex digest is a valid header"),
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    response
}

fn hex_digest(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

fn if_none_match(header: &str, current: &str) -> bool {
    header.trim() == "*"
        || header.split(',').any(|candidate| {
            let candidate = candidate.trim();
            candidate.strip_prefix("W/").unwrap_or(candidate) == current
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn changed_bytes_do_not_match_a_stale_etag() {
        let old = asset_response(&Method::GET, "file.js", &HeaderMap::new(), b"old");
        let old_tag = old.headers()[header::ETAG].clone();
        let mut headers = HeaderMap::new();
        headers.insert(header::IF_NONE_MATCH, old_tag);
        let changed = asset_response(&Method::GET, "file.js", &headers, b"new");
        assert_eq!(changed.status(), StatusCode::OK);
        assert_ne!(
            changed.headers()[header::ETAG],
            headers[header::IF_NONE_MATCH]
        );
        assert_eq!(
            axum::body::to_bytes(changed.into_body(), usize::MAX)
                .await
                .unwrap(),
            "new"
        );
    }
}
