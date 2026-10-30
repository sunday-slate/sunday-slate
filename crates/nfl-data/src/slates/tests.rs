use time::OffsetDateTime;
use time::macros::datetime;

use super::test_support::Fixture;
use crate::slates;
use crate::slates::{Slate, SlateId};
use crate::store;
use crate::{Game, NflData, Season, SeasonType, SeasonWeek, Week};

const BEFORE: OffsetDateTime = datetime!(2025-11-24 04:59:59 UTC);
const MONDAY: OffsetDateTime = datetime!(2025-11-24 05:00 UTC);
const WEDNESDAY: OffsetDateTime = datetime!(2025-11-26 22:00 UTC);
const THURSDAY: OffsetDateTime = datetime!(2025-11-27 12:00 UTC);
const EDT_UTC_MONDAY: OffsetDateTime = datetime!(2025-09-01 00:00 UTC);
const EDT_MONDAY: OffsetDateTime = datetime!(2025-09-01 04:00 UTC);

const WEEK_13: SeasonWeek = SeasonWeek {
    season: Season(2025),
    season_type: SeasonType::Reg,
    week: Week(13),
};
const WEEK_1: SeasonWeek = SeasonWeek {
    season: Season(2025),
    season_type: SeasonType::Reg,
    week: Week(1),
};
const WEEK_16: SeasonWeek = SeasonWeek {
    season: Season(2025),
    season_type: SeasonType::Reg,
    week: Week(16),
};
const BEFORE_WEEK_16: OffsetDateTime = datetime!(2025-12-22 04:59:59 UTC);
const WEEK_16_MONDAY: OffsetDateTime = datetime!(2025-12-22 05:00 UTC);
const XMAS_WEDNESDAY: OffsetDateTime = datetime!(2025-12-24 22:00 UTC);

fn game(id: &str, week: u8, kickoff: Option<OffsetDateTime>) -> Game {
    Game {
        game_id: id.into(),
        season_week: SeasonWeek {
            season: Season(2025),
            season_type: SeasonType::Reg,
            week: Week(week),
        },
        kickoff,
        away_team: "KC".parse().unwrap(),
        home_team: "BUF".parse().unwrap(),
        home_score: None,
        away_score: None,
    }
}

fn week_13_games() -> Vec<Game> {
    vec![
        game("tg_1", 13, Some(datetime!(2025-11-27 17:30 UTC))),
        game("tg_2", 13, Some(datetime!(2025-11-27 21:30 UTC))),
        game("tg_3", 13, Some(datetime!(2025-11-28 01:20 UTC))),
        game("sun_1", 13, Some(datetime!(2025-11-30 17:00 UTC))),
        game("sun_2", 13, Some(datetime!(2025-11-30 20:25 UTC))),
        game("snf", 13, Some(datetime!(2025-12-01 01:20 UTC))),
        game("mnf", 13, Some(datetime!(2025-12-02 01:15 UTC))),
        game("wk12", 12, Some(datetime!(2025-11-23 18:00 UTC))),
        game("wk14", 14, Some(datetime!(2025-12-04 01:15 UTC))),
    ]
}

fn changed_week_13_games() -> Vec<Game> {
    let mut games = week_13_games();
    for game in games.iter_mut() {
        if game.game_id == "sun_1" {
            game.kickoff = Some(datetime!(2025-12-01 01:15 UTC));
        }
        if game.game_id == "mnf" {
            game.kickoff = Some(datetime!(2025-11-30 21:25 UTC));
        }
    }
    games
}

fn rescheduled_after_fix_games() -> Vec<Game> {
    let mut games = week_13_games();
    for game in games.iter_mut() {
        if game.game_id == "sun_1" {
            game.kickoff = Some(datetime!(2025-11-29 21:30 UTC));
        }
    }
    games.push(game("new_sun", 13, Some(datetime!(2025-11-30 21:25 UTC))));
    games
}

fn week_1_games() -> Vec<Game> {
    vec![
        game("op", 1, Some(datetime!(2025-09-05 00:15 UTC))),
        game("sun_1", 1, Some(datetime!(2025-09-07 17:00 UTC))),
        game("sun_2", 1, Some(datetime!(2025-09-07 20:25 UTC))),
        game("snf", 1, Some(datetime!(2025-09-08 00:20 UTC))),
        game("mnf", 1, Some(datetime!(2025-09-09 00:15 UTC))),
    ]
}

fn week_16_games() -> Vec<Game> {
    vec![
        game("xmas", 16, Some(datetime!(2025-12-25 18:00 UTC))),
        game("sun_1", 16, Some(datetime!(2025-12-28 17:00 UTC))),
        game("sun_2", 16, Some(datetime!(2025-12-28 20:25 UTC))),
    ]
}

fn moved_off_christmas_games() -> Vec<Game> {
    let mut games = week_16_games();
    for game in games.iter_mut() {
        if game.game_id == "xmas" {
            game.kickoff = Some(datetime!(2025-12-27 18:00 UTC));
        }
    }
    games
}

/// Reconcile the whole season at an explicit clock, returning every stored
/// slate. This is the only writer of slate rows; lookups below never populate.
async fn populate(nfl: &NflData, now: OffsetDateTime) -> Vec<Slate> {
    slates::populate_at(&nfl.provider, &nfl.store, nfl.season(), now)
        .await
        .expect("populate")
        .into_iter()
        .collect()
}

async fn stored_week(nfl: &NflData, week: SeasonWeek) -> Vec<Slate> {
    slates::by_week(nfl, week).await.expect("stored week")
}

async fn stored_games(nfl: &NflData, id: SlateId) -> Vec<Game> {
    slates::games(nfl, id).await.expect("stored games")
}

async fn tentative_week_13() -> (Fixture, SlateId, SlateId) {
    let fixture = Fixture::connect().await;
    fixture
        .nfl
        .seed_for_test(&[], &week_13_games())
        .await
        .unwrap();
    populate(&fixture.nfl, BEFORE).await;
    let established = stored_week(&fixture.nfl, WEEK_13).await;
    let main_id = slate_named(&established, "Week 13").id;
    let thanksgiving_id = slate_named(&established, "Thanksgiving").id;
    (fixture, main_id, thanksgiving_id)
}

async fn assert_week_13_fixed(nfl: &NflData, main_id: SlateId, thanksgiving_id: SlateId) {
    assert!(
        store::slates::is_fixed(nfl.store.reader(), WEEK_13)
            .await
            .unwrap(),
        "the week's snapshot is fixed"
    );
    assert_eq!(
        store::slates::game_ids(nfl.store.reader(), main_id)
            .await
            .unwrap(),
        ["sun_1", "sun_2"]
    );
    assert_eq!(
        store::slates::game_ids(nfl.store.reader(), thanksgiving_id)
            .await
            .unwrap(),
        ["tg_1", "tg_2", "tg_3"]
    );
}

fn ids(games: &[Game]) -> Vec<&str> {
    games.iter().map(|game| game.game_id.as_str()).collect()
}

fn names(slates: &[Slate]) -> Vec<&str> {
    slates.iter().map(|slate| slate.name.as_str()).collect()
}

fn slate_named<'a>(slates: &'a [Slate], name: &str) -> &'a Slate {
    slates
        .iter()
        .find(|slate| slate.name == name)
        .unwrap_or_else(|| panic!("no slate named {name} in {slates:?}"))
}

#[tokio::test]
async fn tentative_membership_and_stored_kickoff_follow_schedule() {
    let fixture = Fixture::connect().await;
    fixture
        .nfl
        .seed_for_test(&[], &week_13_games())
        .await
        .unwrap();

    populate(&fixture.nfl, BEFORE).await;
    let established = stored_week(&fixture.nfl, WEEK_13).await;
    assert_eq!(
        names(&established),
        ["Thanksgiving", "Week 13"],
        "known kickoffs ascending"
    );
    let main = slate_named(&established, "Week 13");
    assert_eq!(main.kickoff, Some(datetime!(2025-11-30 17:00 UTC)));
    let thanksgiving = slate_named(&established, "Thanksgiving");
    assert_eq!(thanksgiving.kickoff, Some(datetime!(2025-11-27 17:30 UTC)));
    assert_eq!(
        ids(&stored_games(&fixture.nfl, main.id).await),
        ["sun_1", "sun_2"]
    );
    assert_eq!(
        ids(&stored_games(&fixture.nfl, thanksgiving.id).await),
        ["tg_1", "tg_2", "tg_3"]
    );
    assert!(
        !store::slates::is_fixed(fixture.nfl.store.reader(), WEEK_13)
            .await
            .unwrap(),
        "the week stays tentative before its Eastern Monday"
    );

    fixture.replace_games(&changed_week_13_games()).await;

    populate(&fixture.nfl, BEFORE).await;
    let after = stored_week(&fixture.nfl, WEEK_13).await;
    let after_main = slate_named(&after, "Week 13");
    assert_eq!(after_main.id, main.id, "the slate ID does not change");
    assert_eq!(
        after_main.kickoff,
        Some(datetime!(2025-11-30 20:25 UTC)),
        "the stored kickoff follows the schedule"
    );
    assert_eq!(
        ids(&stored_games(&fixture.nfl, after_main.id).await),
        ["sun_2", "mnf"],
        "membership follows the schedule before fixation"
    );
}

#[tokio::test]
async fn eastern_monday_not_utc_monday_fixes_membership() {
    let fixture = Fixture::connect().await;
    fixture
        .nfl
        .seed_for_test(&[], &week_13_games())
        .await
        .unwrap();

    populate(&fixture.nfl, BEFORE).await;
    assert!(
        !store::slates::is_fixed(fixture.nfl.store.reader(), WEEK_13)
            .await
            .unwrap(),
        "23:59:59 Eastern Sunday has not reached the opening Monday"
    );
    populate(&fixture.nfl, MONDAY).await;
    assert!(
        store::slates::is_fixed(fixture.nfl.store.reader(), WEEK_13)
            .await
            .unwrap(),
        "00:00 Eastern Monday fixes the week"
    );

    let edt = Fixture::connect().await;
    edt.nfl.seed_for_test(&[], &week_1_games()).await.unwrap();

    populate(&edt.nfl, EDT_UTC_MONDAY).await;
    assert!(
        !store::slates::is_fixed(edt.nfl.store.reader(), WEEK_1)
            .await
            .unwrap(),
        "UTC Monday midnight is Eastern Sunday in September"
    );
    populate(&edt.nfl, EDT_MONDAY).await;
    assert!(
        store::slates::is_fixed(edt.nfl.store.reader(), WEEK_1)
            .await
            .unwrap(),
        "00:00 Eastern Monday fixes the week"
    );
}

#[tokio::test]
async fn one_population_fixes_every_slate_in_the_week() {
    let (fixture, main_id, thanksgiving_id) = tentative_week_13().await;
    populate(&fixture.nfl, MONDAY).await;
    assert_eq!(
        names(&stored_week(&fixture.nfl, WEEK_13).await),
        ["Thanksgiving", "Week 13"],
        "population covers every slate in the week"
    );
    assert_week_13_fixed(&fixture.nfl, main_id, thanksgiving_id).await;

    let found = slates::by_id(&fixture.nfl, main_id)
        .await
        .unwrap()
        .expect("stored slate");
    assert_eq!(found.id, main_id, "reads answer from storage");

    populate(&fixture.nfl, MONDAY).await;
    assert_week_13_fixed(&fixture.nfl, main_id, thanksgiving_id).await;
}

#[tokio::test]
async fn first_population_after_the_opening_monday_uses_current_schedule() {
    let fixture = Fixture::connect().await;
    fixture
        .nfl
        .seed_for_test(&[], &changed_week_13_games())
        .await
        .unwrap();

    let got = populate(&fixture.nfl, WEDNESDAY).await;
    let main = slate_named(&got, "Week 13");
    assert_eq!(
        ids(&stored_games(&fixture.nfl, main.id).await),
        ["sun_2", "mnf"],
        "the first population captures Wednesday's selection"
    );
    assert_eq!(main.kickoff, Some(datetime!(2025-11-30 20:25 UTC)));
    assert!(
        store::slates::is_fixed(fixture.nfl.store.reader(), WEEK_13)
            .await
            .unwrap(),
        "the first population after the opening Monday fixes the week"
    );
    assert_eq!(
        store::slates::game_ids(fixture.nfl.store.reader(), main.id)
            .await
            .unwrap(),
        ["mnf", "sun_2"]
    );
}

#[tokio::test]
async fn disappearing_holiday_keeps_identity_and_clears_stale_kickoff() {
    let fixture = Fixture::connect().await;
    fixture
        .nfl
        .seed_for_test(&[], &week_16_games())
        .await
        .unwrap();

    populate(&fixture.nfl, BEFORE_WEEK_16).await;
    let established = stored_week(&fixture.nfl, WEEK_16).await;
    assert_eq!(
        names(&established),
        ["Christmas", "Week 16"],
        "known kickoffs ascending"
    );
    let christmas = slate_named(&established, "Christmas");
    assert_eq!(christmas.kickoff, Some(datetime!(2025-12-25 18:00 UTC)));
    let christmas_id = christmas.id;

    fixture.replace_games(&moved_off_christmas_games()).await;

    populate(&fixture.nfl, BEFORE_WEEK_16).await;
    assert!(
        stored_games(&fixture.nfl, christmas_id).await.is_empty(),
        "the holiday game left the Christmas membership"
    );
    let after_move = stored_week(&fixture.nfl, WEEK_16).await;
    let leftover = slate_named(&after_move, "Christmas");
    assert_eq!(leftover.id, christmas_id, "the row and its id survive");
    assert_eq!(
        leftover.kickoff, None,
        "the stale kickoff cleared before fixation"
    );
    assert_eq!(
        slate_named(&after_move, "Week 16").kickoff,
        Some(datetime!(2025-12-28 17:00 UTC))
    );

    let found = slates::by_id(&fixture.nfl, christmas_id)
        .await
        .unwrap()
        .expect("stored slate");
    assert_eq!(found.kickoff, None);

    populate(&fixture.nfl, WEEK_16_MONDAY).await;
    let first_fixed = stored_week(&fixture.nfl, WEEK_16).await;
    let fixed_leftover = slate_named(&first_fixed, "Christmas");
    assert_eq!(fixed_leftover.id, christmas_id);
    assert_eq!(fixed_leftover.kickoff, None);
    assert_eq!(
        slate_named(&first_fixed, "Week 16").kickoff,
        Some(datetime!(2025-12-28 17:00 UTC))
    );
    assert!(
        store::slates::is_fixed(fixture.nfl.store.reader(), WEEK_16)
            .await
            .unwrap()
    );
    assert!(
        store::slates::game_ids(fixture.nfl.store.reader(), christmas_id)
            .await
            .unwrap()
            .is_empty(),
        "the omitted proposal gained no membership"
    );

    populate(&fixture.nfl, XMAS_WEDNESDAY).await;
    let later = stored_week(&fixture.nfl, WEEK_16).await;
    assert_eq!(
        slate_named(&later, "Christmas").clone(),
        fixed_leftover.clone()
    );
    assert_eq!(
        slate_named(&later, "Week 16").clone(),
        slate_named(&first_fixed, "Week 16").clone()
    );
}

#[tokio::test]
async fn rescheduling_after_fix_changes_details_not_membership() {
    let fixture = Fixture::connect().await;
    fixture
        .nfl
        .seed_for_test(&[], &week_13_games())
        .await
        .unwrap();
    let established = populate(&fixture.nfl, MONDAY).await;
    let main_id = slate_named(&established, "Week 13").id;

    fixture.replace_games(&rescheduled_after_fix_games()).await;

    let fixed_ids = store::slates::game_ids(fixture.nfl.store.reader(), main_id)
        .await
        .unwrap();
    assert_eq!(
        fixed_ids,
        ["sun_1", "sun_2"],
        "selected IDs remain unchanged"
    );

    populate(&fixture.nfl, THURSDAY).await;
    let got = stored_games(&fixture.nfl, main_id).await;
    assert_eq!(
        fixed_ids,
        ids(&got),
        "the snapshot, not the rules' new Sunday candidates"
    );
    assert_eq!(
        got[0].kickoff,
        Some(datetime!(2025-11-29 21:30 UTC)),
        "returned Game kickoffs follow the current schedule"
    );
    assert_eq!(
        slates::by_id(&fixture.nfl, main_id)
            .await
            .unwrap()
            .unwrap()
            .kickoff,
        Some(datetime!(2025-11-29 21:30 UTC)),
        "the stored kickoff refreshed from the fixed members"
    );
    assert_eq!(
        slates::by_id(&fixture.nfl, main_id)
            .await
            .unwrap()
            .unwrap()
            .id,
        main_id
    );
}

#[tokio::test]
async fn week_snapshot_is_not_reopened_by_a_backdated_clock() {
    let fixture = Fixture::connect().await;
    fixture
        .nfl
        .seed_for_test(&[], &week_13_games())
        .await
        .unwrap();
    let established = populate(&fixture.nfl, MONDAY).await;
    let main_id = slate_named(&established, "Week 13").id;

    fixture.replace_games(&changed_week_13_games()).await;

    populate(&fixture.nfl, BEFORE).await;
    let got = stored_games(&fixture.nfl, main_id).await;
    assert_eq!(
        ids(&got),
        ["sun_2", "sun_1"],
        "the fixed membership survives a backdated clock"
    );
    assert_eq!(
        got[1].kickoff,
        Some(datetime!(2025-12-01 01:15 UTC)),
        "current schedule details still flow through"
    );
    assert!(
        store::slates::is_fixed(fixture.nfl.store.reader(), WEEK_13)
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn unknown_id_has_no_side_effects() {
    let fixture = Fixture::connect().await;
    fixture
        .nfl
        .seed_for_test(&[], &week_13_games())
        .await
        .unwrap();
    let unknown = SlateId(999_999);

    assert!(
        slates::by_id(&fixture.nfl, unknown)
            .await
            .unwrap()
            .is_none(),
        "absent ids have no metadata"
    );
    assert!(
        slates::games(&fixture.nfl, unknown)
            .await
            .unwrap()
            .is_empty(),
        "absent ids have no games"
    );
    assert!(
        !store::slates::is_fixed(fixture.nfl.store.reader(), WEEK_13)
            .await
            .unwrap(),
        "no week snapshot was created"
    );
    assert!(
        store::slates::for_week(fixture.nfl.store.reader(), WEEK_13)
            .await
            .unwrap()
            .is_empty(),
        "no metadata rows were created"
    );
}
