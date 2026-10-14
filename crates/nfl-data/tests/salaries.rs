use nfl_data::{DfsPosition, NflData, NflDataConfig, SeasonWeek, TeamAbbr};

const CSV: &[u8] = include_bytes!("../../fanduel-data/tests/fixtures/fanduel-export.csv");

#[tokio::test]
async fn facade_imports_reads_and_counts_typed_salaries_for_exact_week() {
    let nfl = NflData::connect(NflDataConfig {
        database_url: "sqlite::memory:".into(),
        fanduel_database_url: "sqlite::memory:".into(),
        ..Default::default()
    })
    .await
    .unwrap();
    let week = SeasonWeek::reg(1);

    let imported = nfl.parse_fanduel_csv(week, CSV).unwrap();
    assert_eq!(imported.len(), 4);
    assert_eq!(imported[0].player.as_ref().unwrap().id, 62239);
    assert_eq!(imported[0].position, DfsPosition::Qb);
    assert!(imported.iter().any(|salary| {
        salary.player.is_none()
            && salary.position == DfsPosition::Dst
            && salary.team == TeamAbbr::NYG
    }));
    nfl.save_fanduel_salaries(&imported).await.unwrap();
    let readback = nfl.salaried_players_for(week).await.unwrap();
    assert_eq!(readback.len(), imported.len());
    assert!(imported.iter().all(|salary| readback.contains(salary)));
    assert!(
        nfl.salaried_players_for(SeasonWeek::reg(2))
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(nfl.salary_counts_by_week().await.unwrap(), vec![(week, 4)]);

    let updated_csv = std::str::from_utf8(CSV)
        .unwrap()
        .replacen(",17,8200,", ",17,8400,", 1);
    let updated = nfl.parse_fanduel_csv(week, updated_csv.as_bytes()).unwrap();
    assert_eq!(updated.len(), 4);
    assert_eq!(updated[0].salary, 8400);
    nfl.save_fanduel_salaries(&updated).await.unwrap();
    let readback = nfl.salaried_players_for(week).await.unwrap();
    assert_eq!(readback.len(), updated.len());
    assert!(updated.iter().all(|salary| readback.contains(salary)));
    assert_eq!(nfl.salary_counts_by_week().await.unwrap(), vec![(week, 4)]);
}

#[tokio::test]
async fn facade_returns_parser_errors() {
    let nfl = NflData::in_memory().await.unwrap();
    let malformed = std::str::from_utf8(CSV)
        .unwrap()
        .replacen(",QB,", ",CPT,", 1);
    assert!(
        nfl.parse_fanduel_csv(SeasonWeek::reg(1), malformed.as_bytes())
            .is_err()
    );
}
