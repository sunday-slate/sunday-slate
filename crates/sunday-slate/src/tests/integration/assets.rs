use image::GenericImageView;

use crate::tests::TestApp;

#[tokio::test]
async fn manifest_served_for_root_install() {
    let app = TestApp::new().await;
    let resp = app.get("/static/manifest.webmanifest").await;
    resp.assert_status_ok();
    assert_eq!(resp.header("content-type"), "application/manifest+json");

    let manifest = resp.json::<serde_json::Value>();
    assert_eq!(manifest["name"], "Sunday Slate");
    assert_eq!(manifest["short_name"], "Sunday Slate");
    assert_eq!(manifest["id"], "/");
    assert_eq!(manifest["start_url"], "/");
    assert_eq!(manifest["scope"], "/");
    assert_eq!(manifest["display"], "standalone");
    assert_eq!(manifest["background_color"], "#FAFAF9");
    assert_eq!(manifest["theme_color"], "#FAFAF9");
    assert_eq!(
        manifest["icons"],
        serde_json::json!([
            {
                "src": crate::assets::versioned("/static/img/app-icon-192.png"),
                "sizes": "192x192",
                "type": "image/png",
                "purpose": "any maskable"
            },
            {
                "src": crate::assets::versioned("/static/img/app-icon-512.png"),
                "sizes": "512x512",
                "type": "image/png",
                "purpose": "any maskable"
            }
        ])
    );
}

#[tokio::test]
async fn app_icons_served_at_declared_sizes() {
    let app = TestApp::new().await;

    for (path, size) in [
        ("/static/img/app-icon-192.png", (192, 192)),
        ("/static/img/app-icon-512.png", (512, 512)),
        ("/static/img/apple-touch-icon.png", (180, 180)),
    ] {
        let resp = app.get(path).await;
        resp.assert_status_ok();
        assert_eq!(resp.header("content-type"), "image/png");
        let bytes = resp.as_bytes();
        assert_eq!(
            image::guess_format(bytes).expect("detect icon format"),
            image::ImageFormat::Png,
            "unexpected image format for {path}"
        );
        let icon = image::load_from_memory(bytes).expect("decode PNG icon");
        assert_eq!(icon.dimensions(), size, "unexpected dimensions for {path}");
    }
}

#[tokio::test]
async fn favicon_served() {
    let app = TestApp::new().await;
    let resp = app.get("/static/img/favicon.svg").await;
    resp.assert_status_ok();
}

#[tokio::test]
async fn css_served() {
    let app = TestApp::new().await;
    let resp = app.get("/static/css/app.css").await;
    resp.assert_status_ok();
}

#[tokio::test]
async fn htmx_served() {
    let app = TestApp::new().await;
    let resp = app.get("/static/vendor/js/htmx.min.js").await;
    resp.assert_status_ok();
}

#[tokio::test]
async fn alpine_served() {
    let app = TestApp::new().await;
    let resp = app.get("/static/vendor/js/alpine.min.js").await;
    resp.assert_status_ok();
}

#[tokio::test]
async fn phosphor_icons_served() {
    let app = TestApp::new().await;
    let resp = app.get("/static/vendor/phosphor/regular.css").await;
    resp.assert_status_ok();
}

#[tokio::test]
async fn font_served() {
    let app = TestApp::new().await;
    let resp = app.get("/static/vendor/fonts/geist.woff2").await;
    resp.assert_status_ok();
}

#[tokio::test]
async fn versioned_css_serves_immutable_and_same_etag() {
    let app = TestApp::new().await;

    let bare = app.get("/static/css/app.css").await;
    bare.assert_status_ok();
    assert_eq!(bare.header("cache-control"), "no-cache");

    let url = crate::assets::versioned("/static/css/app.css");
    assert!(url != "/static/css/app.css");
    let resp = app.get(&url).await;
    resp.assert_status_ok();
    assert_eq!(
        resp.header("cache-control"),
        "public, max-age=31536000, immutable"
    );
    assert_eq!(resp.header("etag"), bare.header("etag"));
}

#[tokio::test]
async fn stale_version_serves_no_cache() {
    let app = TestApp::new().await;
    let resp = app.get("/static/css/app.css?v=deadbeef").await;
    resp.assert_status_ok();
    assert_eq!(resp.header("cache-control"), "no-cache");
}

#[tokio::test]
async fn embedded_references_are_stamped() {
    let app = TestApp::new().await;

    let man = app.get("/static/manifest.webmanifest").await;
    man.assert_status_ok();
    let man = String::from_utf8_lossy(man.as_bytes());
    assert_eq!(man.matches("/static/img/app-icon-192.png?v=").count(), 1);
    assert_eq!(man.matches("/static/img/app-icon-512.png?v=").count(), 1);

    let css = app.get("/static/css/app.css").await;
    let css = String::from_utf8_lossy(css.as_bytes());
    assert!(css.contains("/static/vendor/fonts/geist.woff2?v="));
    assert!(css.contains("/static/vendor/fonts/geist-mono.woff2?v="));

    let reg = app.get("/static/vendor/phosphor/regular.css").await;
    assert!(String::from_utf8_lossy(reg.as_bytes()).contains("./Phosphor.woff2?v="));
    let fill = app.get("/static/vendor/phosphor/fill.css").await;
    assert!(String::from_utf8_lossy(fill.as_bytes()).contains("./Phosphor-Fill.woff2?v="));
}

#[tokio::test]
async fn versioned_leaf_assets_serve() {
    let app = TestApp::new().await;
    for path in [
        "/static/img/app-icon-192.png",
        "/static/vendor/fonts/geist.woff2",
        "/static/vendor/phosphor/Phosphor.woff2",
    ] {
        let url = crate::assets::versioned(path);
        assert_ne!(url, path, "leaf asset must be versioned: {path}");
        let resp = app.get(&url).await;
        resp.assert_status_ok();
    }
}

fn assert_all_static_urls_versioned(body: &str) {
    let mut count = 0;
    for rest in body.split("/static/").skip(1) {
        let url = &rest[..rest.find('"').unwrap_or(rest.len())];
        count += 1;
        let (path, v) = url
            .split_once("?v=")
            .unwrap_or_else(|| panic!("static url must carry ?v: {url}"));
        assert!(!path.is_empty());
        assert_eq!(v.len(), 16, "hex version in {url}");
        assert!(
            v.chars().all(|c| c.is_ascii_hexdigit()),
            "hex version in {url}"
        );
    }
    assert_eq!(count, 10, "layout.html ships exactly ten /static refs");
}

#[tokio::test]
async fn layout_emits_fingerprinted_static_urls() {
    let mut app = TestApp::new().await;
    app.login_admin().await;
    app.clear_cookies();
    let resp = app.get("/login").await;
    resp.assert_status_ok();
    assert_all_static_urls_versioned(&String::from_utf8_lossy(resp.as_bytes()));
}
