use crate::tests::factories::TeamOptions;
use crate::tests::utils::as_rendered;
use crate::tests::{TestApp, factories};

/// Full-page smoke: seeded user logs in, sees the league on the standings.
#[tokio::test]
async fn smoke_homepage() {
    let app = TestApp::new().await;
    let gen_team = factories::team(&app.pool, TeamOptions::default()).await;

    app.login_as(&gen_team.owner).await;

    let resp = app.home().await;
    let body = resp.text();
    assert!(
        body.contains(&as_rendered(&gen_team.team.name)),
        "team visible: {body}"
    );
}

/// A team name is only visible in the page in escaped form. Asserting the raw
/// string made this suite fail whenever the faker happened to produce a name
/// with an apostrophe — roughly one run in several, with no code change.
#[tokio::test]
async fn a_team_name_that_needs_escaping_is_still_found() {
    let app = TestApp::new().await;
    let gen_team = factories::team(
        &app.pool,
        TeamOptions {
            name: Some("O'Conner, Kertzmann & Sons <b>".into()),
            ..Default::default()
        },
    )
    .await;

    app.login_as(&gen_team.owner).await;

    let board = app.get("/standings").await;
    board.assert_status_ok();
    let body = board.text();
    assert!(
        !body.contains(&gen_team.team.name),
        "the raw name is exactly what does NOT appear — that was the bug"
    );
    assert!(
        body.contains(&as_rendered(&gen_team.team.name)),
        "but its rendered form does: {body}"
    );
}
