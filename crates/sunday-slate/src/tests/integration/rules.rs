use crate::tests::TestApp;
use crate::tests::factories::{self, LeagueOptions};
use crate::tests::utils::{as_rendered, href_before};

/// The overflow menu's Rules & Scoring item is how a league member reaches
/// the page: follow it from a rendered page, never a typed URL.
#[tokio::test]
async fn rules_page_is_reachable_from_the_overflow_menu() {
    let app = TestApp::new().await;
    let gen_league = factories::league(&app.pool, LeagueOptions::default()).await;
    app.login_as(&gen_league.commish).await;

    let body = app.get("/standings").await.text();
    assert!(
        body.contains("</i>Rules &amp; Scoring"),
        "the overflow menu should carry a Rules & Scoring item: {body}"
    );

    let href = href_before(&body, "</i>Rules &amp; Scoring");
    let resp = app.get(&href).await;

    resp.assert_status_ok();
    let page = resp.text();
    assert!(
        page.contains(&format!("<title>{}", as_rendered("Rules & Scoring"))),
        "the link should land on the rules page: {page}"
    );
}
