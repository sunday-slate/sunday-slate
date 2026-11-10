//! `GET /` dispatch and shell-level home behavior, exercised through HTTP.

use axum::http::StatusCode;

use crate::tests::TestApp;
use crate::tests::factories::{self, TeamOptions};
use crate::tests::publish_slate;

fn week(week: u8) -> nfl_data::SeasonWeek {
    nfl_data::SeasonWeek {
        season: nfl_data::Season(2026),
        week: nfl_data::Week(week),
    }
}

fn game(id: &str, week: nfl_data::SeasonWeek, kickoff: time::OffsetDateTime) -> nfl_data::Game {
    nfl_data::Game {
        game_id: id.into(),
        season_week: week,
        kickoff: Some(kickoff),
        home_team: nfl_data::TeamAbbr::KC,
        away_team: nfl_data::TeamAbbr::BUF,
        home_score: None,
        away_score: None,
    }
}

#[tokio::test]
async fn a_contest_less_preseason_lands_on_standings() {
    let app = TestApp::new().await;
    let team = factories::team(&app.pool, TeamOptions::default()).await;

    app.login_as(&team.owner).await;

    let resp = app.get("/").await;
    resp.assert_status(StatusCode::SEE_OTHER);
    assert_eq!(resp.header("location").to_str().unwrap(), "/standings");

    // No current slate means no Current navigation target.
    let standings = app.get("/standings").await;
    standings.assert_status_ok();
    assert!(
        !standings.text().contains("ph-football"),
        "the Current tab hides without a snapshot slate"
    );
}

#[sqlx::test]
async fn current_redirects_to_the_snapshot_maximum_slate(pool: sqlx::SqlitePool) {
    let nfl = std::sync::Arc::new(nfl_data::NflData::in_memory().await.unwrap());
    let app = TestApp::from_pool_with_nfl_at(
        pool.clone(),
        nfl,
        time::macros::datetime!(2026-09-14 12:00 UTC),
    )
    .await;
    let team = factories::team(&pool, TeamOptions::default()).await;
    app.login_as(&team.owner).await;

    // Week 1 is live at the pinned clock; week 2 is populated and upcoming.
    let week1 = week(1);
    let week2 = week(2);
    let wk1 = publish_slate(
        &app,
        week1,
        &[game(
            "2026_01_BUF_KC",
            week1,
            time::macros::datetime!(2026-09-13 17:00 UTC),
        )],
        &[],
    )
    .await;
    let wk2 = publish_slate(
        &app,
        week2,
        &[game(
            "2026_02_BUF_KC",
            week2,
            time::macros::datetime!(2026-09-20 17:00 UTC),
        )],
        &[],
    )
    .await;

    let resp = app.get("/").await;
    resp.assert_status(StatusCode::SEE_OTHER);
    assert_eq!(
        resp.header("location").to_str().unwrap(),
        format!("/contests/{}", wk2.0),
        "Current is the greatest populated slate id, not the live kickoff"
    );

    let current = app.get(&format!("/contests/{}", wk2.0)).await;
    current.assert_status_ok();
    assert!(
        current.text().contains(
            "href=\"/\" class=\"flex flex-1 flex-col items-center gap-0.5 py-2 text-[11px] font-semibold text-primary\" aria-current=\"page\""
        ),
        "the Current tab lights on the current slate's page: {}",
        current.text()
    );

    let list = app.get("/contests").await;
    list.assert_status_ok();
    let body = list.text();
    assert!(
        body.contains(&format!("/contests/{}", wk1.0)),
        "the started week remains listed: {body}"
    );
    assert!(
        body.contains(&format!("/contests/{}", wk2.0)),
        "the upcoming current week is listed: {body}"
    );
}

/// A two-day postseason slate is live, locked, and range-formatted; at the
/// next Eastern midnight the owner's open row disappears and the finished
/// contest awaits official stats instead of inventing zeros.
#[sqlx::test]
async fn multiday_slate_status_drives_catalog_details_and_open_rows(pool: sqlx::SqlitePool) {
    let nfl = std::sync::Arc::new(nfl_data::NflData::in_memory().await.unwrap());
    let sunday = time::macros::datetime!(2027-01-10 20:00 UTC);
    let app = TestApp::from_pool_with_nfl_at(pool.clone(), nfl.clone(), sunday).await;
    let team = factories::team(&pool, TeamOptions::default()).await;
    app.login_as(&team.owner).await;

    let wild_card = nfl_data::SeasonWeek {
        season: nfl_data::Season(2026),
        week: nfl_data::Week(19),
    };
    let games = [
        nfl_data::Game {
            game_id: "2026_19_BUF_KC".into(),
            season_week: wild_card,
            kickoff: Some(time::macros::datetime!(2027-01-09 21:00 UTC)),
            home_team: nfl_data::TeamAbbr::KC,
            away_team: nfl_data::TeamAbbr::BUF,
            home_score: None,
            away_score: None,
        },
        nfl_data::Game {
            game_id: "2026_19_GB_DET".into(),
            season_week: wild_card,
            kickoff: Some(time::macros::datetime!(2027-01-10 18:00 UTC)),
            home_team: nfl_data::TeamAbbr::DET,
            away_team: nfl_data::TeamAbbr::GB,
            home_score: None,
            away_score: None,
        },
    ];
    let slate = publish_slate(&app, wild_card, &games, &[]).await;
    assert_eq!(
        slate,
        nfl_data::slates::SlateId(i64::from(wild_card.encode()) * 10 + 1),
        "the published main-kind slate"
    );
    let entry_id = factories::full_entry(&pool, slate, team.team.id, "00-A").await;

    let persisted = app
        .nfl
        .snapshot()
        .slate(&slate)
        .expect("the published slate")
        .clone();
    assert!(persisted.multiday, "Saturday and Sunday span two days");
    assert!(persisted.live(sunday));
    assert!(persisted.locked(sunday));

    let list = app.get("/contests").await;
    list.assert_status_ok();
    let body = list.text();
    assert!(body.contains("Jan 9–10"), "the two-day range: {body}");
    assert!(body.contains(&format!("/contests/{}", slate.0)), "{body}");

    let team_page = app.get(&format!("/teams/{}", team.team.id)).await;
    team_page.assert_status_ok();
    let body = team_page.text();
    assert!(
        body.contains(&format!("href=\"/contests/{}\"", slate.0)),
        "the owner's live row links to the contest: {body}"
    );
    assert!(
        !body.contains("Enter lineup") && !body.contains("Resume lineup"),
        "a live row offers no draft action: {body}"
    );

    let details = app.get(&format!("/contests/{}", slate.0)).await;
    details.assert_status_ok();
    let body = details.text();
    assert!(
        body.contains(&format!("contest-{}-entry-{entry_id}-score", slate.0)),
        "the entry score region renders: {body}"
    );
    assert!(body.contains(&team.team.name), "{body}");
    assert!(!body.contains("Kicks off"), "no upcoming header: {body}");
    assert!(
        !body.contains("Still to enter"),
        "no waiting section: {body}"
    );

    let monday = TestApp::from_pool_with_nfl_at(
        pool.clone(),
        nfl,
        time::macros::datetime!(2027-01-11 05:00 UTC),
    )
    .await;
    monday.login_as(&team.owner).await;

    let team_page = monday.get(&format!("/teams/{}", team.team.id)).await;
    team_page.assert_status_ok();
    let body = team_page.text();
    assert!(
        !body.contains("Open Contests"),
        "the open row is gone: {body}"
    );
    assert!(
        !body.contains(&format!("href=\"/contests/{}\"", slate.0)),
        "no live or actionable row remains: {body}"
    );

    let list = monday.get("/contests").await;
    list.assert_status_ok();
    let body = list.text();
    assert!(body.contains("Jan 9–10"), "{body}");
    assert!(
        body.contains("Final · awaiting official stats"),
        "finished without official stats, not zero-scored: {body}"
    );
}

/// An upcoming slate's stored UTC kickoff renders as its Eastern wall clock in
/// both the details header and the owner's open-row countdown.
#[sqlx::test]
async fn upcoming_utc_kickoff_renders_eastern_header_and_countdown(pool: sqlx::SqlitePool) {
    let nfl = std::sync::Arc::new(nfl_data::NflData::in_memory().await.unwrap());
    let app = TestApp::from_pool_with_nfl_at(
        pool.clone(),
        nfl,
        time::macros::datetime!(2026-09-10 12:00 UTC),
    )
    .await;
    let team = factories::team(&pool, TeamOptions::default()).await;
    app.login_as(&team.owner).await;

    let week1 = week(1);
    let slate = publish_slate(
        &app,
        week1,
        &[game(
            "2026_01_BUF_KC",
            week1,
            time::macros::datetime!(2026-09-13 17:00 UTC),
        )],
        &[],
    )
    .await;

    let details = app.get(&format!("/contests/{}", slate.0)).await;
    details.assert_status_ok();
    let body = details.text();
    assert!(
        body.contains("Kicks off Sun, Sep 13 · 1:00 PM ET"),
        "the header is Eastern, not 5:00 PM UTC: {body}"
    );
    assert!(
        body.contains("Entries lock at kickoff — in 3 days"),
        "{body}"
    );

    let team_page = app.get(&format!("/teams/{}", team.team.id)).await;
    team_page.assert_status_ok();
    let body = team_page.text();
    assert!(
        body.contains("Kicks off Sun, Sep 13 · 1:00 PM ET — locks in 3 days"),
        "the open-row countdown is Eastern: {body}"
    );
}

/// The catalog is every published snapshot slate: a holiday slate and a
/// postseason slate appear alongside their week's main slate.
#[sqlx::test]
async fn catalog_includes_holiday_and_postseason_slates(pool: sqlx::SqlitePool) {
    let nfl = std::sync::Arc::new(nfl_data::NflData::in_memory().await.unwrap());
    let app = TestApp::from_pool_with_nfl_at(
        pool.clone(),
        nfl,
        time::macros::datetime!(2027-01-20 12:00 UTC),
    )
    .await;
    let team = factories::team(&pool, TeamOptions::default()).await;
    app.login_as(&team.owner).await;

    let holiday_week = week(12);
    let thanksgiving = nfl_data::Game {
        game_id: "2026_12_DET_GB".into(),
        season_week: holiday_week,
        kickoff: Some(time::macros::datetime!(2026-11-26 17:00 UTC)),
        home_team: nfl_data::TeamAbbr::GB,
        away_team: nfl_data::TeamAbbr::DET,
        home_score: None,
        away_score: None,
    };
    let sunday = game(
        "2026_12_BUF_KC",
        holiday_week,
        time::macros::datetime!(2026-11-29 17:00 UTC),
    );
    publish_slate(&app, holiday_week, &[thanksgiving, sunday], &[]).await;

    let wild_card = nfl_data::SeasonWeek {
        season: nfl_data::Season(2026),
        week: nfl_data::Week(19),
    };
    let saturday = nfl_data::Game {
        game_id: "2026_19_BUF_KC".into(),
        season_week: wild_card,
        kickoff: Some(time::macros::datetime!(2027-01-09 21:00 UTC)),
        home_team: nfl_data::TeamAbbr::KC,
        away_team: nfl_data::TeamAbbr::BUF,
        home_score: None,
        away_score: None,
    };
    publish_slate(&app, wild_card, &[saturday], &[]).await;

    let list = app.get("/contests").await;
    list.assert_status_ok();
    let body = list.text();
    for name in ["Week 12", "Thanksgiving", "Wild Card"] {
        assert!(body.contains(name), "{name} is in the catalog: {body}");
    }
}
