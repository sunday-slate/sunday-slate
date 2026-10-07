use axum_test::multipart::{MultipartForm, Part};
use http::StatusCode;
use nfl_data::{Game, Player, Season, SeasonType, TeamAbbr as NflTeamAbbr, Week};

use crate::admin::salary_imports::store as import_store;
use crate::tests::TestApp;
use crate::tests::factories::{self, UserOptions};

fn game(id: &str, away: &str, home: &str) -> Game {
    Game {
        gsis_game_id: id.into(),
        season: Season(2025),
        week: Week(1),
        season_type: SeasonType::Reg,
        kickoff: None,
        home_team: NflTeamAbbr(home.into()),
        away_team: NflTeamAbbr(away.into()),
        home_score: None,
        away_score: None,
    }
}
fn player(gsis: &str, name: &str, team: &str) -> Player {
    Player {
        gsis_id: gsis.into(),
        espn_id: None,
        full_name: name.into(),
        first_name: None,
        last_name: None,
        position: Some("WR".into()),
        latest_team: Some(NflTeamAbbr(team.into())),
        headshot_url: None,
    }
}
const HDR: &str = "\"Id\",\"Position\",\"First Name\",\"Nickname\",\"Last Name\",\"FPPG\",\"Played\",\"Salary\",\"Game\",\"Team\",\"Opponent\",\"Injury Indicator\",\"Injury Details\"";

fn slate_csv() -> String {
    // fd id 200's name won't match the seeded "Jaxon Smith-Njigba".
    format!(
        "{HDR}\n\"1-200\",\"WR\",\"J.\",\"\",\"Smith-Njigba\",\"0\",\"0\",\"5000\",\"BUF@NYJ\",\"BUF\",\"NYJ\",\"\",\"\"\n"
    )
}
fn upload_form() -> MultipartForm {
    MultipartForm::new().add_part(
        "csv",
        Part::bytes(slate_csv().into_bytes())
            .file_name("s.csv")
            .mime_type("text/csv"),
    )
}

#[tokio::test]
async fn resolve_then_reimport_auto_matches() {
    let app = TestApp::new().await;
    let admin = factories::user(
        &app.pool,
        UserOptions {
            is_admin: true,
            ..Default::default()
        },
    )
    .await;
    app.login_as(&admin.user).await;
    app.nfl
        .seed_for_test(
            &[player("00-0039337", "Jaxon Smith-Njigba", "BUF")],
            &[game("2025_01_BUF_NYJ", "BUF", "NYJ")],
        )
        .await
        .unwrap();

    // First upload: one unmatched row.
    let loc = app
        .server
        .post("/admin/salary-imports")
        .multipart(upload_form())
        .await
        .header("location")
        .to_str()
        .unwrap()
        .to_string();
    let id: i64 = loc.rsplit('/').next().unwrap().parse().unwrap();
    assert_eq!(
        import_store::unmatched_count(&app.pool, id).await.unwrap(),
        1
    );

    // Resolve it to the real player.
    let row_id = import_store::rows_for(&app.pool, id).await.unwrap()[0].id;
    app.post_htmx(
        &format!("/admin/salary-imports/{id}/rows/{row_id}"),
        "gsis_player_id=00-0039337",
    )
    .await
    .assert_status_ok();

    // Commit.
    app.post(&format!("/admin/salary-imports/{id}/commit"), "")
        .await
        .assert_status(StatusCode::SEE_OTHER);

    // Re-upload the same CSV: the crosswalk now auto-matches — zero unmatched.
    let loc2 = app
        .server
        .post("/admin/salary-imports")
        .multipart(upload_form())
        .await
        .header("location")
        .to_str()
        .unwrap()
        .to_string();
    let id2: i64 = loc2.rsplit('/').next().unwrap().parse().unwrap();
    assert_eq!(
        import_store::unmatched_count(&app.pool, id2).await.unwrap(),
        0,
        "resolution should stick via the crosswalk"
    );
}
