use crate::admin::salary_imports::schedule::resolve_slate_games;
use nfl_data::{Game, Season, SeasonType, SeasonWeek, Week};
use std::collections::BTreeSet;
use time::OffsetDateTime;
use time::macros::datetime;

fn game(id: &str, week: u8, kickoff: OffsetDateTime) -> Game {
    Game {
        game_id: id.into(),
        season_week: nfl_data::SeasonWeek {
            season: Season(2025),
            season_type: SeasonType::Reg,
            week: Week(week),
        },
        kickoff: Some(kickoff),
        home_team: "SEA".parse().unwrap(),
        away_team: "SF".parse().unwrap(),
        home_score: None,
        away_score: None,
    }
}

#[tokio::test]
async fn migrations_remove_the_staged_import_tables() {
    let app = crate::tests::TestApp::new().await;
    let tables: Vec<String> = sqlx::query_scalar("SELECT name FROM sqlite_master WHERE type = 'table' AND name IN ('salary_import_rows', 'salary_imports') ORDER BY name")
        .fetch_all(&app.pool)
        .await
        .unwrap();
    assert!(tables.is_empty(), "staged tables remain: {tables:?}");
}

#[test]
fn schedule_resolution_requires_complete_season_week() {
    let games = vec![
        game("reg", 1, datetime!(2025-09-07 17:00 UTC)),
        game("post", 20, datetime!(2026-01-18 17:00 UTC)),
    ];
    let matchups = BTreeSet::from([("SF".to_owned(), "SEA".to_owned())]);
    let regular = SeasonWeek {
        season: Season(2025),
        season_type: SeasonType::Reg,
        week: Week(1),
    };
    let postseason = SeasonWeek {
        season: Season(2025),
        season_type: SeasonType::Post,
        week: Week(20),
    };
    assert_eq!(
        resolve_slate_games(&games, &regular, &matchups).unwrap()[0].game_id,
        "reg"
    );
    assert!(resolve_slate_games(&games, &postseason, &matchups).is_err());
}
