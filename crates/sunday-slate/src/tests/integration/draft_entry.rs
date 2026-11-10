//! Draft-entry editor: SlateId identity, save-by-slate, and write gates.

use std::sync::Arc;

use sqlx::SqlitePool;
use time::OffsetDateTime;
use time::macros::datetime;

use crate::tests::TestApp;
use crate::tests::factories::{self, TeamOptions};
use crate::tests::publish_slate;

const KICKOFF: OffsetDateTime = datetime!(2026-09-13 17:00 UTC);
const BEFORE_KICKOFF: OffsetDateTime = datetime!(2026-09-12 16:00 UTC);
const AFTER_KICKOFF: OffsetDateTime = datetime!(2026-09-13 18:00 UTC);

fn week() -> nfl_data::SeasonWeek {
    nfl_data::SeasonWeek {
        season: nfl_data::Season(2026),
        week: nfl_data::Week(1),
    }
}

fn game() -> nfl_data::Game {
    nfl_data::Game {
        game_id: "2026_01_BUF_KC".into(),
        season_week: week(),
        kickoff: Some(KICKOFF),
        home_team: nfl_data::TeamAbbr::KC,
        away_team: nfl_data::TeamAbbr::BUF,
        home_score: None,
        away_score: None,
    }
}

fn player(gsis: &str, name: &str, position: &str) -> nfl_data::Player {
    let mut player = factories::player(gsis, name);
    player.position = Some(position.into());
    player
}

fn players() -> Vec<nfl_data::Player> {
    vec![
        player("00-Q", "Quinn Back", "QB"),
        player("00-R1", "Alpha Runner", "RB"),
        player("00-R2", "Beta Runner", "RB"),
        player("00-W1", "Wide One", "WR"),
        player("00-W2", "Wide Two", "WR"),
        player("00-W3", "Wide Three", "WR"),
        player("00-T", "Tight End", "TE"),
        player("00-F", "Flex Runner", "RB"),
    ]
}

async fn app_at(pool: &SqlitePool, nfl: Arc<nfl_data::NflData>, now: OffsetDateTime) -> TestApp {
    TestApp::from_pool_with_nfl_at(pool.clone(), nfl, now).await
}

/// Publish the fixture slate, create a team, and sign its owner in.
async fn setup(
    pool: &SqlitePool,
    now: OffsetDateTime,
) -> (TestApp, i64, nfl_data::slates::SlateId) {
    let nfl = Arc::new(nfl_data::NflData::in_memory().await.unwrap());
    let app = app_at(pool, Arc::clone(&nfl), now).await;
    let team = factories::team(pool, TeamOptions::default()).await;
    let slate = publish_slate(&app, week(), &[game()], &players()).await;
    app.login_as(&team.owner).await;
    (app, team.team.id, slate)
}

const PICKS: [(&str, &str); 8] = [
    ("QB", "00-Q"),
    ("RB1", "00-R1"),
    ("RB2", "00-R2"),
    ("WR1", "00-W1"),
    ("WR2", "00-W2"),
    ("WR3", "00-W3"),
    ("TE", "00-T"),
    ("FLEX", "00-F"),
];

#[sqlx::test]
async fn resume_and_save_a_draft_by_slate_id(pool: SqlitePool) {
    let (app, team_id, slate) = setup(&pool, BEFORE_KICKOFF).await;
    assert_eq!(slate.0, 20260011);

    // A partial draft resumes through the same slate key.
    let response = app
        .post(
            &format!("/contests/{}/draft-entry/QB", slate.0),
            "value=00-Q",
        )
        .await;
    response.assert_status(axum::http::StatusCode::SEE_OTHER);
    let editor = app.get(&format!("/contests/{}/draft-entry", slate.0)).await;
    editor.assert_status_ok();
    assert!(
        editor.text().contains("Week 1"),
        "the editor heading names the slate: {}",
        editor.text()
    );

    for (slot, value) in PICKS.iter().skip(1) {
        app.post(
            &format!("/contests/{}/draft-entry/{slot}", slate.0),
            &format!("value={value}"),
        )
        .await
        .assert_status(axum::http::StatusCode::SEE_OTHER);
    }
    app.post(
        &format!("/contests/{}/draft-entry/DEF", slate.0),
        "value=KC",
    )
    .await
    .assert_status(axum::http::StatusCode::SEE_OTHER);

    let saved = app
        .post(&format!("/contests/{}/draft-entry/save", slate.0), "")
        .await;
    saved.assert_status(axum::http::StatusCode::SEE_OTHER);
    let location = saved.header("location").to_str().unwrap().to_string();
    let entry_id: i64 = location
        .strip_prefix("/entries/")
        .expect("save redirects to the committed entry")
        .parse()
        .unwrap();

    let stored: i64 = sqlx::query_scalar("SELECT slate_id FROM entries WHERE id = ?1")
        .bind(entry_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(stored, 20260011, "the entry persists the slate id");
    let drafts: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM draft_entries WHERE fantasy_team_id = ?1")
            .bind(team_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(drafts, 0, "saving consumes the draft");

    let entry = app.get(&location).await;
    entry.assert_status_ok();
    let body = entry.text();
    assert!(
        body.contains("Alpha Runner"),
        "the saved lineup renders: {body}"
    );
    assert!(
        body.contains("20260011") || body.contains("Week 1"),
        "{body}"
    );
}

#[sqlx::test]
async fn unknown_slate_write_is_404_and_inserts_nothing(pool: SqlitePool) {
    let (app, _team_id, _slate) = setup(&pool, BEFORE_KICKOFF).await;

    let response = app
        .post("/contests/20260099/draft-entry/QB", "value=00-Q")
        .await;
    response.assert_status(axum::http::StatusCode::NOT_FOUND);

    let drafts: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM draft_entries")
        .fetch_one(&pool)
        .await
        .unwrap();
    let entries: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM entries")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!((drafts, entries), (0, 0));
}

#[sqlx::test]
async fn locked_slate_mutations_are_forbidden_and_keep_stored_picks(pool: SqlitePool) {
    let nfl = Arc::new(nfl_data::NflData::in_memory().await.unwrap());
    let before = app_at(&pool, Arc::clone(&nfl), BEFORE_KICKOFF).await;
    let team = factories::team(&pool, TeamOptions::default()).await;
    let slate = publish_slate(&before, week(), &[game()], &players()).await;
    before.login_as(&team.owner).await;
    before
        .post(
            &format!("/contests/{}/draft-entry/QB", slate.0),
            "value=00-Q",
        )
        .await
        .assert_status(axum::http::StatusCode::SEE_OTHER);

    let after = app_at(&pool, nfl, AFTER_KICKOFF).await;
    after.login_as(&team.owner).await;
    for (slot, body) in [
        ("QB", "value=00-R1".to_string()),
        ("RB1", "value=00-R1".to_string()),
    ] {
        let response = after
            .post(&format!("/contests/{}/draft-entry/{slot}", slate.0), &body)
            .await;
        response.assert_status(axum::http::StatusCode::FORBIDDEN);
    }
    after
        .post(&format!("/contests/{}/draft-entry/QB/clear", slate.0), "")
        .await
        .assert_status(axum::http::StatusCode::FORBIDDEN);
    after
        .post(&format!("/contests/{}/draft-entry/save", slate.0), "")
        .await
        .assert_status(axum::http::StatusCode::FORBIDDEN);

    let qb: Option<String> =
        sqlx::query_scalar("SELECT qb_gsis_id FROM draft_entries WHERE fantasy_team_id = ?1")
            .bind(team.team.id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        qb.as_deref(),
        Some("00-Q"),
        "locked writes leave picks intact"
    );
    let entries: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM entries")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(entries, 0);
}
