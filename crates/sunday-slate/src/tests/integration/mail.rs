use http::StatusCode;

use crate::mail::OutgoingEmail;
use crate::tests::TestApp;
use crate::tests::utils::*;

fn test_email(subject: &str) -> OutgoingEmail {
    OutgoingEmail {
        from: "test@example.com".to_string(),
        to: "user@example.com".to_string(),
        subject: subject.to_string(),
        html: format!("<p>{subject}</p>"),
        text: subject.to_string(),
    }
}

#[tokio::test]
async fn dev_inbox_page_returns_200() {
    let app = TestApp::new().await;

    let response = app.get("/_dev/mail").await;

    response.assert_status_ok();
    let body_str = response.text();
    assert!(
        body_str.contains("Dev inbox"),
        "inbox page should render: {body_str}"
    );
}

#[tokio::test]
async fn dev_inbox_shows_empty_state() {
    let app = TestApp::new().await;

    let response = app.get("/_dev/mail").await;

    response.assert_status_ok();
    let body_str = response.text();
    assert!(
        body_str.contains("No emails captured yet"),
        "inbox should show empty state: {body_str}"
    );
}

#[tokio::test]
async fn dev_inbox_not_mounted_when_smtp_configured() {
    let app = TestApp::new_with_mailer(failing_mailer()).await;

    let response = app.get("/_dev/mail").await;

    response.assert_status(StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn dev_inbox_clear_empties_store() {
    let app = TestApp::new().await;

    app.mailer.send(test_email("first")).await.expect("send 1");
    app.mailer.send(test_email("second")).await.expect("send 2");

    let response = app.get("/_dev/mail").await;
    response.assert_status_ok();
    let body_str = response.text();
    assert!(
        body_str.contains("first"),
        "should show first email: {body_str}"
    );
    assert!(
        body_str.contains("second"),
        "should show second email: {body_str}"
    );

    let response = app.post("/_dev/mail", "").await;
    assert!(response.status_code().is_redirection(), "expected redirect");

    let response = app.get("/_dev/mail").await;
    response.assert_status_ok();
    let body_str = response.text();
    assert!(
        body_str.contains("No emails captured yet"),
        "inbox should be empty after clear: {body_str}"
    );
}

#[tokio::test]
async fn dev_inbox_detail_escapes_email_html_in_srcdoc() {
    let app = TestApp::new().await;

    app.mailer
        .send(OutgoingEmail {
            from: "test@example.com".to_string(),
            to: "user@example.com".to_string(),
            subject: "xss probe".to_string(),
            html: r#"<script>alert("xss")</script>"#.to_string(),
            text: "plain".to_string(),
        })
        .await
        .expect("send");

    let response = app.get("/_dev/mail/0").await;

    response.assert_status_ok();
    let body_str = response.text();

    // HTML-escaped inside srcdoc — raw angle brackets must never appear.
    let numeric = body_str.contains("&#60;script&#62;");
    let named = body_str.contains("&lt;script&gt;");
    assert!(
        numeric || named,
        "email HTML should be escaped in srcdoc: {body_str}"
    );
    assert!(
        !body_str.contains("<script>"),
        "raw <script> must never reach the response: {body_str}"
    );
}

#[tokio::test]
async fn dev_inbox_detail_returns_404_for_missing_index() {
    let app = TestApp::new().await;

    let response = app.get("/_dev/mail/999").await;

    response.assert_status(StatusCode::NOT_FOUND);
}

/// htmx: Clear All answers with HX-Redirect instead of a 3xx.
#[tokio::test]
async fn dev_inbox_clear_htmx_returns_hx_redirect() {
    let app = TestApp::new().await;
    app.mailer.send(test_email("first")).await.expect("send");

    let response = app.post_htmx("/_dev/mail", "").await;

    response.assert_status_ok();
    assert_eq!(
        response.header("hx-redirect"),
        "/_dev/mail",
        "htmx clear should HX-Redirect to the inbox"
    );
}
