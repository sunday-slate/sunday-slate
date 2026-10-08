use nfl_data::{
    Game, NflData, NflDataConfig, Player, Season, SeasonType, TeamAbbr, Week, WeeklyRosterEntry,
};

fn player() -> Player {
    Player {
        gsis_id: "00-P".into(),
        espn_id: Some("42".into()),
        full_name: "Player Table".into(),
        first_name: None,
        last_name: None,
        position: Some("RB".into()),
        latest_team: Some(TeamAbbr("KC".into())),
        headshot_url: Some("https://example.test/player.png".into()),
    }
}

fn game() -> Game {
    Game {
        gsis_game_id: "2025_01_BUF_KC".into(),
        season: Season(2025),
        week: Week(1),
        season_type: SeasonType::Reg,
        kickoff: None,
        home_team: TeamAbbr("KC".into()),
        away_team: TeamAbbr("BUF".into()),
        home_score: None,
        away_score: None,
    }
}

#[tokio::test]
async fn empty_provider_reads_preserve_missing_result_behavior() {
    let nfl = NflData::in_memory().await.unwrap();
    assert!(nfl.games(Season(2025)).await.unwrap().is_empty());
    assert!(nfl.players().await.unwrap().is_empty());
    assert!(nfl.players_by_gsis(&["missing"]).await.unwrap().is_empty());
    assert!(
        nfl.weekly_roster_entries_by_gsis(&["missing"])
            .await
            .unwrap()
            .is_empty()
    );
    assert!(nfl.identify(&["missing"]).await.unwrap().is_empty());
    let team = TeamAbbr("KC".into());
    assert!(
        nfl.weekly_roster(Season(2025), Week(1), &team)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        nfl.player_week_stats(Season(2025), Week(1))
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        nfl.team_week_stats(Season(2025), Week(1))
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        nfl.data_revision().await.unwrap(),
        nfl.data_revision().await.unwrap()
    );
}

#[tokio::test]
async fn seeded_records_survive_reopening_and_identity_prefers_players() {
    let dir = tempfile::tempdir().unwrap();
    let config = NflDataConfig {
        database_url: format!("sqlite://{}/cache.db", dir.path().display()),
        fanduel_database_url: format!("sqlite://{}/fanduel.db", dir.path().display()),
        ..Default::default()
    };
    let nfl = NflData::connect(config.clone()).await.unwrap();
    nfl.seed_for_test(&[player()], &[game()]).await.unwrap();
    let roster = |id: &str| WeeklyRosterEntry {
        season: Season(2025),
        week: Week(1),
        team: TeamAbbr("BUF".into()),
        gsis_id: Some(id.into()),
        espn_id: None,
        full_name: "Roster Name".into(),
        last_name: None,
        position: Some("WR".into()),
    };
    nfl.seed_weekly_roster_for_test(&[roster("00-P"), roster("00-R")])
        .await
        .unwrap();
    drop(nfl);
    let nfl = NflData::connect(config).await.unwrap();
    assert_eq!(nfl.games(Season(2025)).await.unwrap(), [game()]);
    assert_eq!(nfl.players().await.unwrap(), [player()]);
    let players = nfl
        .players_by_gsis(&["00-P", "00-P", "missing"])
        .await
        .unwrap();
    assert_eq!(players.len(), 1);
    assert_eq!(players["00-P"], player());
    assert_eq!(
        nfl.weekly_roster(Season(2025), Week(1), &TeamAbbr("BUF".into()))
            .await
            .unwrap()
            .len(),
        2
    );
    let identities = nfl.identify(&["00-P", "00-R", "missing"]).await.unwrap();
    assert_eq!(identities.len(), 2);
    assert_eq!(identities["00-P"].name, "Player Table");
    assert_eq!(identities["00-P"].headshot_url, player().headshot_url);
    assert_eq!(identities["00-R"].name, "Roster Name");
    assert_eq!(identities["00-R"].team, Some(TeamAbbr("BUF".into())));
    assert!(identities["00-R"].headshot_url.is_none());
    assert_eq!(
        nfl.data_revision().await.unwrap(),
        nfl.data_revision().await.unwrap()
    );
}
