//! The pick flow, walked as a user: contest → editor → per-slot pick pages →
//! save → entry, and discard from the fresh and committed-editor states.

use axum::http::StatusCode;
use nfl_data::Game;
use sqlx::SqlitePool;
use time::OffsetDateTime;
use time::macros::datetime;

use crate::entries::service::abbreviated_name;
use crate::tests::factories::{self, GeneratedTeam, TeamOptions};
use crate::tests::test_app::TestApp;
use crate::tests::utils::{as_rendered, form_body};

/// The one game on the contest's slate.
const SLATE_GAME: &str = "2025_01_BUF_KC";

/// Kickoff: Sunday 2025-09-07 13:00 ET.
const KICKOFF: OffsetDateTime = datetime!(2025-09-07 17:00 UTC);

/// One hour before kickoff: the slate is open.
const BEFORE_KICKOFF: OffsetDateTime = datetime!(2025-09-07 16:00 UTC);

/// The nine slots, roster order.
const SLOTS: [&str; 9] = ["QB", "RB1", "RB2", "WR1", "WR2", "WR3", "TE", "FLEX", "DEF"];

fn scheduled_game() -> Game {
    Game {
        kickoff: Some(KICKOFF),
        ..factories::game(SLATE_GAME, 1)
    }
}

/// A team and an open "Week 1" contest on `SLATE_GAME`, every candidate
/// priced at $5,000 — eight players plus the KC defense, so any nine-slot
/// lineup saves under the $60,000 cap.
async fn open_contest(pool: &SqlitePool, app: &TestApp) -> (GeneratedTeam, i64, String) {
    let fixture = factories::team(pool, TeamOptions::default()).await;
    let contest = factories::contest_with_games(pool, "Week 1", &[SLATE_GAME]).await;
    for (gsis, position) in [
        ("00-Q", "QB"),
        ("00-R1", "RB"),
        ("00-R2", "RB"),
        ("00-W1", "WR"),
        ("00-W2", "WR"),
        ("00-W3", "WR"),
        ("00-T", "TE"),
        ("00-F", "RB"),
    ] {
        sqlx::query(
            "INSERT INTO nfl_player_salaries (gsis_game_id, gsis_player_id, team_abbr, dfs_position, salary)
             VALUES (?1, ?2, 'KC', ?3, 5000)",
        )
        .bind(SLATE_GAME)
        .bind(gsis)
        .bind(position)
        .execute(pool)
        .await
        .unwrap();
    }
    sqlx::query(
        "INSERT INTO nfl_player_salaries (gsis_game_id, gsis_player_id, team_abbr, dfs_position, salary)
         VALUES (?1, NULL, 'KC', 'DST', 5000)",
    )
    .bind(SLATE_GAME)
    .execute(pool)
    .await
    .unwrap();
    let players = [
        factories::player("00-Q", "Jaxon Smith-Njigba"),
        factories::player("00-R1", "Alpha Runner"),
        factories::player("00-R2", "Beta Runner"),
        factories::player("00-W1", "Wide One"),
        factories::player("00-W2", "Wide Two"),
        factories::player("00-W3", "Wide Three"),
        factories::player("00-T", "Tight End"),
        factories::player("00-F", "Flex Runner"),
    ];
    let full_name = players[0].full_name.clone();
    app.nfl
        .seed_for_test(&players, &[scheduled_game()])
        .await
        .unwrap();
    (fixture, contest, full_name)
}

/// The value of the first pick form on a pick page: `gsis_id` for a player
/// slot, `team_abbr` for DEF.
fn first_pick_value(html: &str, def: bool) -> String {
    let needle = if def {
        r#"name="team_abbr" value=""#
    } else {
        r#"name="gsis_id" value=""#
    };
    html.split(needle)
        .nth(1)
        .and_then(|s| s.split('"').next())
        .expect("a pick form offers a value")
        .to_string()
}

/// The first rendered `href` whose value equals `link` — the exact URL the
/// page offers, asserted to exist and used to follow it.
fn rendered_link<'a>(html: &'a str, link: &str) -> &'a str {
    let needle = format!(r#"href="{link}""#);
    let i = html.find(&needle).expect("rendered link");
    &html[i + r#"href=""#.len()..i + needle.len() - 1]
}

/// How many draft rows a team holds in a contest.
async fn draft_rows(pool: &SqlitePool, contest: i64, team: i64) -> i64 {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM draft_entries WHERE contest_id = ?1 AND fantasy_team_id = ?2",
    )
    .bind(contest)
    .bind(team)
    .fetch_one(pool)
    .await
    .unwrap()
}

#[sqlx::test]
async fn enter_lineup_pick_save_reaches_each_step_from_output(pool: SqlitePool) {
    let app = TestApp::from_pool_at(pool.clone(), BEFORE_KICKOFF).await;
    let (fixture, contest, full_name) = open_contest(&pool, &app).await;
    app.login_as(&fixture.owner).await;

    // The contest page offers the editor link.
    let body = app.get(&format!("/contests/{contest}")).await.text();
    assert!(body.contains("Enter lineup"), "{body}");
    let editor = rendered_link(&body, &format!("/contests/{contest}/draft-entry")).to_string();

    // The fresh editor: Save disabled, every slot links to its pick page.
    let body = app.get(&editor).await.text();
    assert!(body.contains("Save Entry"), "{body}");
    assert!(
        body.contains(r#"btn-primary btn-block" disabled"#),
        "Save is disabled while the lineup is empty: {body}"
    );
    for slot in SLOTS {
        assert!(
            body.contains(&format!(r#"href="/contests/{contest}/draft-entry/{slot}""#)),
            "{slot} links from the editor: {body}"
        );
    }

    // Fill each slot with the pick page's first offer, walking the editor
    // between picks.
    for (i, slot) in SLOTS.iter().enumerate() {
        let pick_url = format!("/contests/{contest}/draft-entry/{slot}");
        let body = app.get(&editor).await.text();
        let pick_path = rendered_link(&body, &pick_url);
        if i < 8 {
            assert!(
                body.contains(r#"btn-primary btn-block" disabled"#),
                "Save stays disabled while filling: {body}"
            );
        }

        let pick_page = app.get(pick_path).await;
        pick_page.assert_status_ok();
        let pick_body = pick_page.text();
        if slot == &"QB" {
            let full = as_rendered(&full_name);
            let abbr = as_rendered(&abbreviated_name(&full_name));
            assert!(
                pick_body.contains("pick-name--long"),
                "long QB names carry the compact-name class: {pick_body}"
            );
            for (class, name) in [
                ("pick-name-full", full.as_str()),
                ("pick-name-abbr", abbr.as_str()),
            ] {
                assert!(
                    pick_body.contains(class) && pick_body.contains(name),
                    "{class} should render {name}: {pick_body}"
                );
            }
        }
        let value = first_pick_value(&pick_body, slot == &"DEF");
        let field = if slot == &"DEF" {
            "team_abbr"
        } else {
            "gsis_id"
        };
        let resp = app.post(pick_path, &form_body(&[(field, &value)])).await;
        resp.assert_status(StatusCode::SEE_OTHER);
        assert_eq!(
            resp.header("location").to_str().unwrap(),
            format!("/contests/{contest}/draft-entry"),
            "{slot} pick lands back on the editor"
        );
    }

    // Full lineup: Save is enabled, and committing lands on the new entry.
    let body = app.get(&editor).await.text();
    assert!(
        !body.contains(r#"btn-primary btn-block" disabled"#),
        "Save is enabled once every slot is filled: {body}"
    );
    let resp = app
        .post(&format!("/contests/{contest}/draft-entry/save"), "")
        .await;
    resp.assert_status(StatusCode::SEE_OTHER);
    let loc = resp.header("location").to_str().unwrap().to_string();
    assert!(
        loc.starts_with("/entries/"),
        "save lands on the entry: {loc}"
    );
    let entry_id: i64 = loc.trim_start_matches("/entries/").parse().unwrap();

    // The owner's pre-kickoff preview, with Edit back into the editor.
    let body = app.get(&loc).await.text();
    assert!(body.contains("Week 1 Lineup"), "{body}");
    assert!(
        body.contains(&format!(r#"href="/contests/{contest}/draft-entry""#)),
        "the preview offers Edit: {body}"
    );

    // Committed: one entry, nine slots, the draft row gone.
    let slots: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM entry_slots WHERE entry_id = ?1")
        .bind(entry_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(slots, 9);
    assert_eq!(draft_rows(&pool, contest, fixture.team.id).await, 0);
}

#[sqlx::test]
async fn discard_abandons_the_draft_and_lands_back(pool: SqlitePool) {
    let app = TestApp::from_pool_at(pool.clone(), BEFORE_KICKOFF).await;
    let (fixture, contest, _) = open_contest(&pool, &app).await;
    app.login_as(&fixture.owner).await;

    // (a) No committed entry: discard drops the draft and returns to the
    // contest.
    let body = app.get(&format!("/contests/{contest}")).await.text();
    let editor = rendered_link(&body, &format!("/contests/{contest}/draft-entry")).to_string();
    app.get(&editor).await.assert_status_ok();
    let resp = app
        .post(&format!("/contests/{contest}/draft-entry/discard"), "")
        .await;
    resp.assert_status(StatusCode::SEE_OTHER);
    assert_eq!(
        resp.header("location").to_str().unwrap(),
        format!("/contests/{contest}")
    );
    assert_eq!(draft_rows(&pool, contest, fixture.team.id).await, 0);

    // (b) A committed entry: the contest pins the viewer's row into its
    // lineup, the entry page offers Edit back into the editor, and discard
    // still lands back on the contest, leaving the committed entry intact.
    let entry_id = factories::full_entry(&pool, contest, fixture.team.id, "00-A").await;
    let body = app.get(&format!("/contests/{contest}")).await.text();
    assert!(
        !body.contains("Enter lineup"),
        "an entrant gets no enter link: {body}"
    );
    let entry_link = rendered_link(&body, &format!("/entries/{entry_id}")).to_string();
    app.get(&entry_link).await.assert_status_ok();
    let body = app.get(&entry_link).await.text();
    assert!(body.contains("Edit"), "{body}");
    let editor = rendered_link(&body, &format!("/contests/{contest}/draft-entry")).to_string();
    app.get(&editor).await.assert_status_ok();

    let resp = app
        .post(&format!("/contests/{contest}/draft-entry/discard"), "")
        .await;
    resp.assert_status(StatusCode::SEE_OTHER);
    assert_eq!(
        resp.header("location").to_str().unwrap(),
        format!("/contests/{contest}"),
        "discard always lands back on the contest"
    );
    assert_eq!(draft_rows(&pool, contest, fixture.team.id).await, 0);
    let entries: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM entries WHERE id = ?1")
        .bind(entry_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(entries, 1, "discard keeps the committed entry");
}

#[sqlx::test]
async fn editor_shows_injury_badge(pool: SqlitePool) {
    let app = TestApp::from_pool_at(pool.clone(), BEFORE_KICKOFF).await;
    let (fixture, contest, _) = open_contest(&pool, &app).await;
    factories::full_entry(&pool, contest, fixture.team.id, "00-Q").await;

    // The coordinator's designation map, published for the slate tuple.
    app.state.injuries.publish(
        (
            nfl_data::Season(2025),
            nfl_data::Week(1),
            nfl_data::SeasonType::Reg,
        ),
        crate::injuries::SlateInjuries {
            by_gsis: std::collections::HashMap::from([(
                crate::entries::NflPlayerId("00-Q".into()),
                crate::injuries::InjuryDesignation::Questionable,
            )]),
            report_date: None,
            attempted_at: Some(BEFORE_KICKOFF),
        },
    );

    app.login_as(&fixture.owner).await;
    let body = app
        .get(&format!("/contests/{contest}/draft-entry"))
        .await
        .text();

    let chip = body
        .find(r#"aria-label="Questionable""#)
        .expect("Q chip renders");
    assert!(
        body.contains(r#"title="Questionable""#) && body.contains(">Q</span>"),
        "the badge carries its short code and full name: {body}"
    );
    // Slot labels drop ordinals, so anchor the RB1 row by its unique swap
    // route: the chip must sit after the QB row and before the RB1 swap link.
    let qb_swap = body
        .find(&format!(r#""/contests/{contest}/draft-entry/QB""#))
        .expect("QB swap link");
    let rb1_swap = body
        .find(&format!(r#""/contests/{contest}/draft-entry/RB1""#))
        .expect("RB1 swap link");
    assert!(
        qb_swap < chip && chip < rb1_swap,
        "the chip sits inside its slot row header: {body}"
    );
    assert_eq!(
        body.matches(r#""badge badge"#).count(),
        1,
        "exactly one chip on the page: {body}"
    );
    assert!(
        !body.contains("badge badge-error"),
        "the defense row carries no badge: {body}"
    );
}

#[sqlx::test]
async fn injury_badge_reached_from_rendered_links(pool: SqlitePool) {
    let app = TestApp::from_pool_at(pool.clone(), BEFORE_KICKOFF).await;
    let (fixture, contest, _) = open_contest(&pool, &app).await;
    let entry_id = factories::full_entry(&pool, contest, fixture.team.id, "00-Q").await;
    app.state.injuries.publish(
        (
            nfl_data::Season(2025),
            nfl_data::Week(1),
            nfl_data::SeasonType::Reg,
        ),
        crate::injuries::SlateInjuries {
            by_gsis: std::collections::HashMap::from([(
                crate::entries::NflPlayerId("00-Q".into()),
                crate::injuries::InjuryDesignation::Questionable,
            )]),
            report_date: None,
            attempted_at: Some(BEFORE_KICKOFF),
        },
    );
    app.login_as(&fixture.owner).await;

    // Contest page → entry page → editor, parsing each hop from rendered HTML.
    let body = app.get(&format!("/contests/{contest}")).await.text();
    assert!(
        !body.contains("Enter lineup"),
        "an entrant gets no enter link: {body}"
    );
    let entry_link = rendered_link(&body, &format!("/entries/{entry_id}")).to_string();
    let body = app.get(&entry_link).await.text();
    assert!(body.contains("Edit"), "{body}");
    let editor = rendered_link(&body, &format!("/contests/{contest}/draft-entry")).to_string();

    let body = app.get(&editor).await.text();
    assert!(
        body.contains(r#"aria-label="Questionable""#) && body.contains(">Q</span>"),
        "the editor shows the Q chip: {body}"
    );
}

#[sqlx::test]
async fn picker_rows_show_the_report_designation(pool: SqlitePool) {
    let app = TestApp::from_pool_at(pool.clone(), BEFORE_KICKOFF).await;
    let (fixture, contest, _) = open_contest(&pool, &app).await;
    app.state.injuries.publish(
        (
            nfl_data::Season(2025),
            nfl_data::Week(1),
            nfl_data::SeasonType::Reg,
        ),
        crate::injuries::SlateInjuries {
            by_gsis: std::collections::HashMap::from([
                (
                    crate::entries::NflPlayerId("00-Q".into()),
                    crate::injuries::InjuryDesignation::Questionable,
                ),
                (
                    crate::entries::NflPlayerId("00-R1".into()),
                    crate::injuries::InjuryDesignation::Out,
                ),
            ]),
            report_date: None,
            attempted_at: Some(BEFORE_KICKOFF),
        },
    );
    app.login_as(&fixture.owner).await;

    // Editor → QB picker, via the rendered swap link.
    let editor_body = app
        .get(&format!("/contests/{contest}/draft-entry"))
        .await
        .text();
    let qb_picker =
        rendered_link(&editor_body, &format!("/contests/{contest}/draft-entry/QB")).to_string();
    let body = app.get(&qb_picker).await.text();
    assert!(
        body.contains(r#"aria-label="Questionable""#) && body.contains(">Q</span>"),
        "the QB picker shows the injured candidate's chip: {body}"
    );

    // RB1 picker: one candidate marked Out, the rest clean. Back through the
    // editor for its swap link.
    let rb1_picker = rendered_link(
        &editor_body,
        &format!("/contests/{contest}/draft-entry/RB1"),
    )
    .to_string();
    let body = app.get(&rb1_picker).await.text();
    let chip = body.find(r#"aria-label="Out""#).expect("Out chip renders");
    let alpha = body.find("Alpha Runner").expect("injured candidate listed");
    let beta = body.find("Beta Runner").expect("healthy candidate listed");
    assert!(
        chip > alpha && chip < beta,
        "the chip sits inside Alpha Runner's row, not Beta Runner's: {body}"
    );
    assert_eq!(
        body.matches("badge badge-warning").count(),
        0,
        "only the Out designation renders in this list: {body}"
    );
}
