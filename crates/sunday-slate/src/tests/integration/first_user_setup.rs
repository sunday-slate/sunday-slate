use crate::tests::TestApp;
use crate::tests::utils::*;
use http::StatusCode;

#[tokio::test]
async fn fresh_install_redirects_to_setup() {
    let app = TestApp::new().await;

    let resp = app.get("/").await;
    resp.assert_status(StatusCode::SEE_OTHER);
    assert_eq!(resp.header("location"), "/setup");

    for path in &["/login", "/forgot-password", "/reset-password"] {
        let resp = app.get(path).await;
        assert_eq!(
            resp.status_code(),
            StatusCode::SEE_OTHER,
            "{path} should redirect to /setup"
        );
        assert_eq!(
            resp.header("location"),
            "/setup",
            "{path} should redirect to /setup"
        );
    }
}

#[tokio::test]
async fn fresh_install_allows_static_assets() {
    let app = TestApp::new().await;

    let resp = app.get("/static/css/app.css").await;

    // The asset service returns 200 or 304; either is fine as long as it's
    // not a redirect to /setup.
    let location = resp
        .maybe_header("location")
        .map(|v| v.to_str().unwrap().to_string());
    assert_ne!(
        location.as_deref(),
        Some("/setup"),
        "static assets must not redirect to /setup"
    );
}

#[tokio::test]
async fn full_setup_journey_starting_from_root() {
    let app = TestApp::new().await;

    let initial = app.get("/").await;
    assert!(
        initial.status_code().is_redirection(),
        "fresh install should redirect, got {}",
        initial.status_code()
    );
    let setup_url = initial.header("location");
    let setup_url = setup_url.to_str().unwrap();
    assert_eq!(setup_url, "/setup");

    let setup_page = app.get(setup_url).await;
    setup_page.assert_status_ok();
    let body = setup_page.text();
    assert!(
        body.contains(r#"action="/setup""#),
        "setup form must be present: {body}"
    );

    let post_response = app
        .post(
            "/setup",
            &form_body(&[
                ("email", "commissioner@example.com"),
                ("password", "hunter22"),
                ("password_confirm", "hunter22"),
            ]),
        )
        .await;
    assert!(
        post_response.status_code().is_redirection(),
        "setup POST should redirect, got {}",
        post_response.status_code()
    );
    let home_url = post_response.header("location");
    let home_url = home_url.to_str().unwrap();
    assert_eq!(home_url, "/");

    // The jar carries the session created by POST /setup.
    let home = app.get(home_url).await;
    home.assert_status(StatusCode::SEE_OTHER);
    assert_eq!(
        home.header("location"),
        "/leagues/new",
        "after setup the user must create a league"
    );
}
