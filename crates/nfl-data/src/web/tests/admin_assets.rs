use axum::{
    Router,
    http::{Method, StatusCode},
    routing::get,
};
use axum_test::TestServer;
use nfl_data::{NflData, admin_router};

async fn app() -> Router {
    let nfl = std::sync::Arc::new(NflData::in_memory().await.unwrap());
    Router::new().nest("/nfl-data-admin", admin_router::<()>(nfl))
}

#[tokio::test]
async fn landing_page_owns_its_shell_and_urls() {
    let server = TestServer::new(app().await);
    let response = server.get("/nfl-data-admin").await;
    assert_eq!(response.status_code(), StatusCode::OK);
    let body = response.text();
    assert!(body.contains("<!doctype html>"));
    assert!(body.contains("href=\"/nfl-data-admin/nflverse\""));
    assert!(body.contains("href=\"/admin\""));
    assert!(body.contains("/nfl-data-admin/static/css/admin.css"));
    assert!(body.contains("/nfl-data-admin/static/vendor/js/htmx.min.js"));
    assert!(!body.contains("href=\"/static/"));
    assert!(!body.contains("src=\"/static/"));
    assert!(!body.to_lowercase().contains("league navigation"));
    assert!(!body.contains("alpine"));
    assert!(!body.contains("sse"));
}

#[tokio::test]
async fn router_composes_with_unrelated_host_state() {
    #[derive(Clone)]
    struct HostState;

    let nfl = std::sync::Arc::new(NflData::in_memory().await.unwrap());
    let router = Router::<HostState>::new()
        .route("/host", get(|| async { "host" }))
        .merge(admin_router::<HostState>(nfl))
        .with_state(HostState);
    let response = TestServer::new(router).get("/").await;
    assert_eq!(response.status_code(), StatusCode::OK);
}

#[tokio::test]
async fn assets_support_get_head_and_conditional_revalidation() {
    let server = TestServer::new(app().await);
    for (path, content_type) in [
        ("css/admin.css", "text/css"),
        ("vendor/js/htmx.min.js", "javascript"),
        ("vendor/fonts/geist.woff2", "font/woff2"),
        ("vendor/fonts/geist-mono.woff2", "font/woff2"),
    ] {
        let path = format!("/nfl-data-admin/static/{path}");
        let response = server.get(&path).await;
        assert_eq!(response.status_code(), StatusCode::OK, "{path}");
        let etag = response.header("etag").to_str().unwrap().to_owned();
        let asset = response.as_bytes().to_vec();
        let asset_length = asset.len();
        assert_eq!(
            response
                .header("content-length")
                .to_str()
                .unwrap()
                .parse::<usize>()
                .unwrap(),
            asset_length
        );
        assert!(etag.starts_with('"') && etag.ends_with('"'));
        assert!(
            response
                .header("content-type")
                .to_str()
                .unwrap()
                .contains(content_type)
        );
        assert_eq!(response.header("cache-control"), "no-cache");

        for tag in [
            etag.clone(),
            format!("W/{etag}"),
            format!("\"stale\", {etag}"),
            "*".into(),
        ] {
            for method in ["GET", "HEAD"] {
                let conditional = server
                    .method(Method::from_bytes(method.as_bytes()).unwrap(), &path)
                    .add_header("if-none-match", tag.clone())
                    .await;
                assert_eq!(conditional.header("etag"), etag);
                assert_eq!(conditional.header("cache-control"), "no-cache");
                assert!(conditional.as_bytes().is_empty());
            }
        }
        for method in ["GET", "HEAD"] {
            for matching_tag in [etag.clone(), format!("W/{etag}")] {
                let conditional = server
                    .method(Method::from_bytes(method.as_bytes()).unwrap(), &path)
                    .add_header("if-none-match", "\"stale\"")
                    .add_header("if-none-match", matching_tag)
                    .await;
                assert_eq!(conditional.status_code(), StatusCode::NOT_MODIFIED);
                assert_eq!(conditional.header("etag"), etag);
                assert_eq!(conditional.header("cache-control"), "no-cache");
                assert!(conditional.as_bytes().is_empty());
            }
        }
        let stale_get = server
            .get(&path)
            .add_header("if-none-match", "\"stale\"")
            .add_header("if-none-match", "\"also-stale\"")
            .await;
        assert_eq!(stale_get.status_code(), StatusCode::OK);
        assert_eq!(stale_get.as_bytes().as_ref(), asset.as_slice());
        let stale_head = server
            .method(Method::HEAD, &path)
            .add_header("if-none-match", "\"stale\"")
            .add_header("if-none-match", "\"also-stale\"")
            .await;
        assert_eq!(stale_head.status_code(), StatusCode::OK);
        assert!(stale_head.as_bytes().is_empty());
        assert_eq!(
            stale_head
                .header("content-length")
                .to_str()
                .unwrap()
                .parse::<usize>()
                .unwrap(),
            asset_length
        );
        let head = server.method(Method::HEAD, &path).await;
        assert_eq!(head.status_code(), StatusCode::OK);
        assert!(head.as_bytes().is_empty());
        assert_eq!(
            head.header("content-length")
                .to_str()
                .unwrap()
                .parse::<usize>()
                .unwrap(),
            asset_length
        );
    }
}

#[tokio::test]
async fn vendor_licenses_are_embedded_and_notices_identify_upstream_terms() {
    let server = TestServer::new(app().await);
    let htmx_path = "/nfl-data-admin/static/vendor/js/LICENSE.txt";
    let htmx = server.get(htmx_path).await;
    assert_eq!(htmx.status_code(), StatusCode::OK, "{htmx_path}");
    let htmx = htmx.text();
    for marker in [
        "Zero-Clause BSD",
        "Permission to use, copy, modify, and/or distribute this software",
        "THE SOFTWARE IS PROVIDED",
        "LOSS OF USE, DATA OR PROFITS",
    ] {
        assert!(
            htmx.contains(marker),
            "missing HTMX license marker: {marker}"
        );
    }

    let font_path = "/nfl-data-admin/static/vendor/fonts/OFL.txt";
    let ofl = server.get(font_path).await;
    assert_eq!(ofl.status_code(), StatusCode::OK, "{font_path}");
    let ofl = ofl.text();
    for marker in [
        "Copyright 2024 The Geist Project Authors",
        "SIL Open Font License, Version 1.1",
        "SIL OPEN FONT LICENSE Version 1.1 - 26 February 2007",
        "PERMISSION & CONDITIONS",
        "THE FONT SOFTWARE IS PROVIDED",
    ] {
        assert!(
            ofl.contains(marker),
            "missing Geist license marker: {marker}"
        );
    }

    let notices = include_str!("../assets/THIRD_PARTY_NOTICES.md");
    for marker in [
        "HTMX 4.0.0",
        "Zero-Clause BSD (0BSD)",
        "Copyright 2024 The Geist Project Authors",
        "SIL Open Font License 1.1",
    ] {
        assert!(notices.contains(marker), "missing notice marker: {marker}");
    }
    assert!(!notices.contains("BSD 2-Clause"));
}

#[tokio::test]
async fn unknown_and_traversal_asset_paths_are_not_served() {
    let server = TestServer::new(app().await);
    for path in [
        "/nfl-data-admin/static/missing.js",
        "/nfl-data-admin/static/../Cargo.toml",
        "/nfl-data-admin/static/%2e%2e/Cargo.toml",
    ] {
        assert_ne!(
            server.get(path).await.status_code(),
            StatusCode::OK,
            "{path}"
        );
    }
}

#[tokio::test]
async fn stylesheet_and_fonts_use_only_the_admin_namespace() {
    let server = TestServer::new(app().await);
    let css = server.get("/nfl-data-admin/static/css/admin.css").await;
    assert_eq!(css.status_code(), StatusCode::OK);
    let body = css.text();
    for font in ["geist.woff2", "geist-mono.woff2"] {
        let url = format!("/nfl-data-admin/static/vendor/fonts/{font}");
        assert!(body.contains(&url));
        assert_eq!(server.get(&url).await.status_code(), StatusCode::OK);
    }
}
