use nfl_data::{NflData, NflDataConfig, Player, Season, TeamAbbr, Week, WeeklyRosterEntry};

fn player() -> Player {
    Player {
        gsis_id: "00-P".into(),
        espn_id: Some("42".into()),
        full_name: "Player Table".into(),
        first_name: None,
        last_name: None,
        position: Some("RB".into()),
        latest_team: Some(TeamAbbr::KC),
        headshot_url: Some("https://example.test/player.png".into()),
    }
}

#[tokio::test]
async fn empty_nflverse_reads_preserve_missing_result_behavior() {
    let nfl = NflData::in_memory().await.unwrap();
    assert!(nfl.players().await.unwrap().is_empty());
    assert!(
        nfl.identify(&[nfl_model::NflPlayerId("missing".into())])
            .await
            .unwrap()
            .is_empty()
    );
    let team = TeamAbbr::KC;
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
async fn seeded_records_preserve_identity_and_prefer_players() {
    let config = NflDataConfig {
        nflverse_database_url: "sqlite::memory:".into(),
        fanduel_database_url: "sqlite::memory:".into(),
        database_url: "sqlite::memory:".into(),
        ..Default::default()
    };
    let nfl = NflData::connect(config).await.unwrap();
    nfl.seed_for_test(&[player()], &[]).await.unwrap();
    let roster = |id: &str| WeeklyRosterEntry {
        season: Season(2025),
        week: Week(1),
        team: TeamAbbr::BUF,
        gsis_id: Some(id.into()),
        espn_id: None,
        full_name: "Roster Name".into(),
        last_name: None,
        position: Some("WR".into()),
    };
    nfl.seed_weekly_roster_for_test(&[roster("00-P"), roster("00-R")])
        .await
        .unwrap();
    assert_eq!(nfl.players().await.unwrap(), [player()]);
    assert_eq!(
        nfl.weekly_roster(Season(2025), Week(1), &TeamAbbr::BUF)
            .await
            .unwrap()
            .len(),
        2
    );
    let identities = nfl
        .identify(&[
            nfl_model::NflPlayerId("00-P".into()),
            nfl_model::NflPlayerId("00-R".into()),
            nfl_model::NflPlayerId("missing".into()),
        ])
        .await
        .unwrap();
    assert_eq!(identities.len(), 2);
    assert_eq!(
        identities[&nfl_model::NflPlayerId("00-P".into())].name,
        "Player Table"
    );
    assert_eq!(
        identities[&nfl_model::NflPlayerId("00-P".into())].headshot_url,
        player().headshot_url
    );
    assert_eq!(
        identities[&nfl_model::NflPlayerId("00-R".into())].name,
        "Roster Name"
    );
    assert_eq!(
        identities[&nfl_model::NflPlayerId("00-R".into())].team,
        Some(TeamAbbr::BUF)
    );
    assert!(
        identities[&nfl_model::NflPlayerId("00-R".into())]
            .headshot_url
            .is_none()
    );
    assert_eq!(
        nfl.data_revision().await.unwrap(),
        nfl.data_revision().await.unwrap()
    );
}
