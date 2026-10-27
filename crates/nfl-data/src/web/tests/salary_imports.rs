use axum::http::StatusCode;

use super::salary_fixture::{
    REPRESENTATIVE_CSV, admin_state, salary_upload_form, seed_representative_slate,
};

fn regular_week_2025(week: u8) -> crate::SeasonWeek {
    crate::SeasonWeek {
        season: crate::Season(2025),
        season_type: crate::SeasonType::Reg,
        week: crate::Week(week),
    }
}

#[tokio::test]
async fn import_rejects_unscheduled_matchup_without_poisoning_corrected_upload() {
    let state = admin_state().await;
    seed_representative_slate(&state.nfl).await;
    let week1 = regular_week_2025(1);

    let incorrect_csv = REPRESENTATIVE_CSV.replacen("BUF@NYJ,BUF,NYJ", "BUF@MIA,BUF,MIA", 1);
    state
        .server
        .post("/nfl-data-admin/salary-imports")
        .multipart(salary_upload_form(1, &incorrect_csv))
        .await
        .assert_status(StatusCode::BAD_REQUEST);
    assert!(
        state
            .nfl
            .salaried_players_for(week1)
            .await
            .unwrap()
            .is_empty(),
        "the rejected upload must not persist partial rows"
    );

    let corrected = state
        .server
        .post("/nfl-data-admin/salary-imports")
        .multipart(salary_upload_form(1, REPRESENTATIVE_CSV))
        .await;
    corrected.assert_status(StatusCode::SEE_OTHER);
    assert_eq!(
        corrected.header("location").to_str().unwrap(),
        "/nfl-data-admin/salary-imports/2025001"
    );
    assert_eq!(
        state.nfl.salaried_players_for(week1).await.unwrap().len(),
        4
    );
    let links = state.nfl.store.links().await.unwrap();
    assert_eq!(links.get(&62239).map(String::as_str), Some("00-1"));
    assert_eq!(links.get(&80796).map(String::as_str), Some("00-2"));
    assert!(
        !links.contains_key(&99999),
        "the unmatched fd id stays unlinked"
    );

    let week1_page = state
        .server
        .get("/nfl-data-admin/salary-imports/2025001")
        .await;
    week1_page.assert_text_contains("4 source salaries · 1 unresolved");
    week1_page.assert_text_contains("Rookie Wideout");

    state
        .server
        .post("/nfl-data-admin/salary-imports")
        .multipart(salary_upload_form(1, REPRESENTATIVE_CSV))
        .await
        .assert_status(StatusCode::SEE_OTHER);
    assert_eq!(
        state.nfl.salaried_players_for(week1).await.unwrap().len(),
        4
    );
    assert_eq!(
        state.nfl.store.links().await.unwrap(),
        links,
        "the repeated upload keeps the same source rows and links"
    );
}

#[tokio::test]
async fn link_applies_globally_without_reimport_and_price_updates_flow_through() {
    let state = admin_state().await;
    seed_representative_slate(&state.nfl).await;
    let week2 = regular_week_2025(2);

    let week2_csv = REPRESENTATIVE_CSV.replacen("BUF@NYJ,BUF,NYJ", "BUF@MIA,BUF,MIA", 1);
    state
        .server
        .post("/nfl-data-admin/salary-imports")
        .multipart(salary_upload_form(2, &week2_csv))
        .await
        .assert_status(StatusCode::SEE_OTHER);

    state
        .server
        .post("/nfl-data-admin/salary-imports/2025002/players/62239/resolve")
        .form(&[("gsis_player_id", "00-1")])
        .await
        .assert_status(StatusCode::SEE_OTHER);

    state
        .server
        .post("/nfl-data-admin/salary-imports")
        .multipart(salary_upload_form(1, REPRESENTATIVE_CSV))
        .await
        .assert_status(StatusCode::SEE_OTHER);

    let week1_page = state
        .server
        .get("/nfl-data-admin/salary-imports/2025001")
        .await;
    week1_page.assert_text_contains("4 source salaries · 1 unresolved");
    week1_page.assert_text_contains("Rookie Wideout");
    assert!(
        !week1_page.text().contains("Josh Allen"),
        "the persisted link resolves Josh Allen without re-importing"
    );

    let mut updated = state.nfl.salaried_players_for(week2).await.unwrap();
    for row in &mut updated {
        if row.player.as_ref().is_some_and(|p| p.id == 62239) {
            row.salary = 9000;
        }
    }
    state.nfl.save_fanduel_salaries(&updated).await.unwrap();

    let priced = state.nfl.salaried_players_for(week2).await.unwrap();
    assert_eq!(priced.len(), 4);
    assert_eq!(
        priced
            .iter()
            .find(|row| row.player.as_ref().is_some_and(|p| p.id == 62239))
            .expect("Josh Allen source row")
            .salary,
        9000,
        "the salary bump flows through the source rows"
    );
    let links = state.nfl.store.links().await.unwrap();
    assert_eq!(links.get(&62239).map(String::as_str), Some("00-1"));
}
