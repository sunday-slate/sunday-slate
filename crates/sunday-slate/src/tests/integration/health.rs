use crate::tests::TestApp;
use crate::tests::factories::{self, UserOptions};
use http::StatusCode;

#[tokio::test]
async fn healthz_returns_ok_without_a_session() {
    let app = TestApp::new().await;

    let resp = app.get("/healthz").await;

    resp.assert_status_ok();
    assert_eq!(resp.text(), "ok");
}

#[tokio::test]
async fn healthz_is_not_behind_the_login_guard() {
    let app = TestApp::new().await;
    factories::user(&app.pool, UserOptions::default()).await;

    let protected = app.get("/standings").await;
    assert_eq!(
        protected.status_code(),
        StatusCode::TEMPORARY_REDIRECT,
        "guard precondition: /standings should redirect an anonymous visitor"
    );
    assert!(
        protected
            .header("location")
            .to_str()
            .unwrap()
            .starts_with("/login"),
        "guard precondition: /standings should redirect to the login page"
    );

    app.get("/healthz").await.assert_status_ok();
}

#[tokio::test]
async fn healthz_serves_a_fresh_install_with_no_users() {
    let app = TestApp::new().await;

    let resp = app.get("/healthz").await;

    resp.assert_status_ok();
    assert_eq!(
        resp.maybe_header("location")
            .map(|v| v.to_str().unwrap().to_string()),
        None,
        "the health probe must not redirect to /setup on a fresh install"
    );
}

#[tokio::test]
async fn healthz_does_not_start_a_session() {
    let app = TestApp::new().await;

    let resp = app.get("/healthz").await;

    resp.assert_status_ok();
    assert!(
        resp.maybe_header("set-cookie").is_none(),
        "the health probe must not create a session"
    );
}
