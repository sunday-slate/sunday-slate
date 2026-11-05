use crate::tests::TestApp;

#[tokio::test]
async fn healthz_answers_an_anonymous_fresh_install_without_a_session() {
    let app = TestApp::new().await;

    let resp = app.get("/healthz").await;

    resp.assert_status_ok();
    assert_eq!(resp.text(), "ok");
    assert_eq!(
        resp.maybe_header("location")
            .map(|v| v.to_str().unwrap().to_string()),
        None,
        "the health probe must not redirect to /setup on a fresh install"
    );
    assert!(
        resp.maybe_header("set-cookie").is_none(),
        "the health probe must not create a session"
    );
}
