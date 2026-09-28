use nfl_data::{NflData, NflDataConfig};

#[tokio::test]
async fn connect_creates_database_and_schema() {
    let dir = tempfile::tempdir().unwrap();
    let config = NflDataConfig {
        database_url: format!("sqlite://{}/nfl-data.db", dir.path().display()),
        ..Default::default()
    };

    let nfl = NflData::connect(config).await.unwrap();

    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE '_sqlx%' ORDER BY name",
    )
    .fetch_all(nfl.reader())
    .await
    .unwrap();
    assert_eq!(
        tables,
        [
            "games",
            "player_week_stats",
            "players",
            "roster_entries",
            "sync_state",
            "team_week_stats",
            "weekly_roster_entries"
        ]
    );
}

#[tokio::test]
async fn in_memory_is_empty_and_queryable() {
    let nfl = nfl_data::NflData::in_memory().await.expect("in_memory");
    assert!(
        nfl.games(nfl_data::Season(2025))
            .await
            .expect("games")
            .is_empty()
    );
    assert!(nfl.players().await.expect("players").is_empty());
}
