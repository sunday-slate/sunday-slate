//! `GET /` dispatch and shell-level home behavior, exercised through HTTP.

use axum::http::StatusCode;

use crate::tests::TestApp;
use crate::tests::factories::{self, TeamOptions};

#[tokio::test]
async fn a_contest_less_preseason_lands_on_standings() {
    let app = TestApp::new().await;
    let team = factories::team(&app.pool, TeamOptions::default()).await;

    app.login_as(&team.owner).await;

    let resp = app.get("/").await;
    resp.assert_status(StatusCode::SEE_OTHER);
    assert_eq!(resp.header("location").to_str().unwrap(), "/standings");
}
