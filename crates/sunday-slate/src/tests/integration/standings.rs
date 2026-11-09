//! Standings: fantasy teams ranked by sum-of-top-10, computed from entries
//! and nfl-data week stats.

use sqlx::SqlitePool;
use time::macros::datetime;

use crate::standings::service::standings_for_league;
use crate::tests::factories::{self, TeamOptions};
use crate::tests::nfl::mocked_nfl;
use crate::tests::publish_slate;
use crate::tests::test_app::TestApp;

#[sqlx::test]
async fn pre_week_one_lists_every_team_without_scores_or_ranks(pool: SqlitePool) {
    let (nfl, _mock) = mocked_nfl(nfl_data::Season(2025)).await;
    let app =
        TestApp::from_pool_with_nfl_at(pool.clone(), nfl, datetime!(2025-09-06 12:00 UTC)).await;
    let league = factories::bare_league(&pool, None).await;
    let entered = factories::team(
        &pool,
        TeamOptions {
            league: Some(league.clone()),
            name: Some("Zulu United".into()),
            owner_name: Some("Zoe".into()),
            ..Default::default()
        },
    )
    .await;
    let _unentered = factories::team(
        &pool,
        TeamOptions {
            league: Some(league.clone()),
            name: Some("Alpha Aces".into()),
            owner_name: Some("Amy".into()),
            ..Default::default()
        },
    )
    .await;

    let week = nfl_data::SeasonWeek {
        season: nfl_data::Season(2025),
        season_type: nfl_data::SeasonType::Reg,
        week: nfl_data::Week(1),
    };
    let wk1 = publish_slate(
        &app,
        week,
        &[factories::game_at(
            "2025_01_BUF_KC",
            1,
            datetime!(2025-09-07 17:00 UTC),
        )],
        &[],
    )
    .await;
    factories::full_entry(&pool, wk1, entered.team.id, "00-A").await;

    app.login_as(&entered.owner).await;
    let resp = app.get("/standings").await;
    resp.assert_status_ok();
    let html = resp.text();

    let alpha = html.find("Alpha Aces").expect("unentered team present");
    let zulu = html.find("Zulu United").expect("entered team present");
    assert!(alpha < zulu, "unscored teams sort by name: {html}");
    assert!(html.contains("Amy"), "unentered owner present: {html}");
    assert!(html.contains("Zoe"), "entered owner present: {html}");
    assert!(
        !html.contains("0.00"),
        "preseason total stays blank: {html}"
    );
    assert!(!html.contains(" win"), "preseason wins stay blank: {html}");

    let rows = standings_for_league(&app.state(), entered.league.id)
        .await
        .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].name, "Alpha Aces");
    assert_eq!(rows[1].name, "Zulu United");
    for row in rows {
        assert!(row.rank.is_none(), "{} has no rank", row.name);
        assert!(row.total.is_none(), "{} has no total", row.name);
        assert_eq!(row.wins, 0, "{} has no wins", row.name);
    }
}
