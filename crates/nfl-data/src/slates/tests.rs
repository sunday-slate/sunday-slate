//! Deterministic facade tests: lazy Monday fixation, separate game retrieval,
//! and the identity and persistence invariants around weekly snapshots.

use time::OffsetDateTime;
use time::macros::datetime;

use super::test_support::Fixture;
use crate::store;
use crate::{Game, NflData, Season, SeasonType, SeasonWeek, Slate, SlateId, Week};

/// One second before the Eastern Monday that opens week 13: lookups stay
/// tentative.
const BEFORE: OffsetDateTime = datetime!(2025-11-24 04:59:59 UTC);
/// Eastern Monday midnight: the instant the week-13 snapshot opens.
const MONDAY: OffsetDateTime = datetime!(2025-11-24 05:00 UTC);
/// Wednesday of week 13, after the opening Monday.
const WEDNESDAY: OffsetDateTime = datetime!(2025-11-26 22:00 UTC);
/// Thursday of week 13, after fixation.
const THURSDAY: OffsetDateTime = datetime!(2025-11-27 12:00 UTC);
/// UTC Monday midnight in September: Eastern Sunday, because EDT runs until
/// November.
const EDT_UTC_MONDAY: OffsetDateTime = datetime!(2025-09-01 00:00 UTC);
/// 04:00 UTC on a September Monday: already Eastern Monday.
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
/// The Christmas week of the 2025 season: a December 25 game and main games
/// on December 28, all with known kickoffs.
const WEEK_16: SeasonWeek = SeasonWeek {
    season: Season(2025),
    season_type: SeasonType::Reg,
    week: Week(16),
};
/// One second before the Eastern Monday that opens week 16: lookups stay
/// tentative.
const BEFORE_WEEK_16: OffsetDateTime = datetime!(2025-12-22 04:59:59 UTC);
/// Eastern Monday midnight: the instant the week-16 snapshot opens.
const WEEK_16_MONDAY: OffsetDateTime = datetime!(2025-12-22 05:00 UTC);
/// Wednesday of week 16, after the week-16 fixation.
const XMAS_WEDNESDAY: OffsetDateTime = datetime!(2025-12-24 22:00 UTC);

/// A fixture game with irrelevant teams and no scores: the facade's membership
/// depends only on the exact week and the kickoff's Eastern time.
fn game(id: &str, week: u8, kickoff: Option<OffsetDateTime>) -> Game {
    Game {
        gsis_game_id: id.into(),
        season: Season(2025),
        week: Week(week),
        season_type: SeasonType::Reg,
        kickoff,
        away_team: "KC".parse().unwrap(),
        home_team: "BUF".parse().unwrap(),
        home_score: None,
        away_score: None,
    }
}

/// The week-13 schedule before any change: Thanksgiving on November 27 and
/// main games on November 30, plus out-of-window and out-of-week games.
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

/// The week-13 schedule with a main game moved out of the Sunday window and a
/// Monday-night game moved into it.
fn changed_week_13_games() -> Vec<Game> {
    let mut games = week_13_games();
    for game in games.iter_mut() {
        if game.gsis_game_id == "sun_1" {
            game.kickoff = Some(datetime!(2025-12-01 01:15 UTC));
        }
        if game.gsis_game_id == "mnf" {
            game.kickoff = Some(datetime!(2025-11-30 21:25 UTC));
        }
    }
    games
}

/// The week-13 schedule after fixation: a selected game moved to Saturday and
/// a new Sunday game added.
fn rescheduled_after_fix_games() -> Vec<Game> {
    let mut games = week_13_games();
    for game in games.iter_mut() {
        if game.gsis_game_id == "sun_1" {
            game.kickoff = Some(datetime!(2025-11-29 21:30 UTC));
        }
    }
    games.push(game("new_sun", 13, Some(datetime!(2025-11-30 21:25 UTC))));
    games
}

/// The week-1 schedule in the EDT season: an opening Thursday and main games
/// on September 7, so the week opens on Eastern Monday September 1.
fn week_1_games() -> Vec<Game> {
    vec![
        game("op", 1, Some(datetime!(2025-09-05 00:15 UTC))),
        game("sun_1", 1, Some(datetime!(2025-09-07 17:00 UTC))),
        game("sun_2", 1, Some(datetime!(2025-09-07 20:25 UTC))),
        game("snf", 1, Some(datetime!(2025-09-08 00:20 UTC))),
        game("mnf", 1, Some(datetime!(2025-09-09 00:15 UTC))),
    ]
}

/// The week-16 schedule: one game on December 25 Eastern and main games on
/// December 28.
fn week_16_games() -> Vec<Game> {
    vec![
        game("xmas", 16, Some(datetime!(2025-12-25 18:00 UTC))),
        game("sun_1", 16, Some(datetime!(2025-12-28 17:00 UTC))),
        game("sun_2", 16, Some(datetime!(2025-12-28 20:25 UTC))),
    ]
}

/// The week-16 schedule with the holiday game rescheduled off December 25 to
/// Saturday December 27: the rule stops proposing Christmas while the rest of
/// the schedule stays complete and known.
fn moved_off_christmas_games() -> Vec<Game> {
    let mut games = week_16_games();
    for game in games.iter_mut() {
        if game.gsis_game_id == "xmas" {
            game.kickoff = Some(datetime!(2025-12-27 18:00 UTC));
        }
    }
    games
}

/// A fixture whose week-13 slates were established tentatively, with the two
/// canonical slate ids.
async fn tentative_week_13() -> (Fixture, SlateId, SlateId) {
    let fixture = Fixture::connect().await;
    fixture
        .nfl
        .seed_for_test(&[], &week_13_games())
        .await
        .unwrap();
    let established = fixture.nfl.slates_at(WEEK_13, BEFORE).await.unwrap();
    let main_id = slate_named(&established, "Week 13").id;
    let thanksgiving_id = slate_named(&established, "Thanksgiving").id;
    (fixture, main_id, thanksgiving_id)
}

/// Both week-13 memberships are stored and the week's snapshot is fixed.
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
    games
        .iter()
        .map(|game| game.gsis_game_id.as_str())
        .collect()
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

    let established = fixture.nfl.slates_at(WEEK_13, BEFORE).await.unwrap();
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
        ids(&fixture.nfl.slate_games_at(main.id, BEFORE).await.unwrap()),
        ["sun_1", "sun_2"]
    );
    assert_eq!(
        ids(&fixture
            .nfl
            .slate_games_at(thanksgiving.id, BEFORE)
            .await
            .unwrap()),
        ["tg_1", "tg_2", "tg_3"]
    );
    assert!(
        !store::slates::is_fixed(fixture.nfl.store.reader(), WEEK_13)
            .await
            .unwrap(),
        "the week stays tentative before its Eastern Monday"
    );

    // A schedule change before fixation: a main game moves out of the Sunday
    // window and a Monday-night game moves into it.
    fixture.replace_games(&changed_week_13_games()).await;

    let after = fixture.nfl.slates_at(WEEK_13, BEFORE).await.unwrap();
    let after_main = slate_named(&after, "Week 13");
    assert_eq!(after_main.id, main.id, "the slate ID does not change");
    assert_eq!(
        after_main.kickoff,
        Some(datetime!(2025-11-30 20:25 UTC)),
        "the stored kickoff follows the schedule"
    );
    assert_eq!(
        ids(&fixture
            .nfl
            .slate_games_at(after_main.id, BEFORE)
            .await
            .unwrap()),
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

    fixture.nfl.slates_at(WEEK_13, BEFORE).await.unwrap();
    assert!(
        !store::slates::is_fixed(fixture.nfl.store.reader(), WEEK_13)
            .await
            .unwrap(),
        "23:59:59 Eastern Sunday has not reached the opening Monday"
    );
    fixture.nfl.slates_at(WEEK_13, MONDAY).await.unwrap();
    assert!(
        store::slates::is_fixed(fixture.nfl.store.reader(), WEEK_13)
            .await
            .unwrap(),
        "00:00 Eastern Monday fixes the week"
    );

    // EDT companion: in September the Eastern Monday starts at 04:00 UTC, so
    // UTC Monday midnight is still Eastern Sunday.
    let edt = Fixture::connect().await;
    edt.nfl.seed_for_test(&[], &week_1_games()).await.unwrap();

    edt.nfl.slates_at(WEEK_1, EDT_UTC_MONDAY).await.unwrap();
    assert!(
        !store::slates::is_fixed(edt.nfl.store.reader(), WEEK_1)
            .await
            .unwrap(),
        "UTC Monday midnight is Eastern Sunday in September"
    );
    edt.nfl.slates_at(WEEK_1, EDT_MONDAY).await.unwrap();
    assert!(
        store::slates::is_fixed(edt.nfl.store.reader(), WEEK_1)
            .await
            .unwrap(),
        "00:00 Eastern Monday fixes the week"
    );
}

#[tokio::test]
async fn each_lookup_entry_point_fixes_every_slate_in_the_week() {
    // Listing fixes both memberships.
    let (fixture, main_id, thanksgiving_id) = tentative_week_13().await;
    fixture.nfl.slates_at(WEEK_13, MONDAY).await.unwrap();
    assert_week_13_fixed(&fixture.nfl, main_id, thanksgiving_id).await;

    // A metadata lookup by id fixes both memberships.
    let (fixture, main_id, thanksgiving_id) = tentative_week_13().await;
    let found = fixture
        .nfl
        .slate_at(main_id, MONDAY)
        .await
        .unwrap()
        .expect("stored slate");
    assert_eq!(found.id, main_id);
    assert_week_13_fixed(&fixture.nfl, main_id, thanksgiving_id).await;

    // A game lookup fixes both memberships.
    let (fixture, main_id, thanksgiving_id) = tentative_week_13().await;
    assert_eq!(
        ids(&fixture.nfl.slate_games_at(main_id, MONDAY).await.unwrap()),
        ["sun_1", "sun_2"]
    );
    assert_week_13_fixed(&fixture.nfl, main_id, thanksgiving_id).await;
}

#[tokio::test]
async fn late_first_lookup_uses_current_schedule() {
    let fixture = Fixture::connect().await;
    // The schedule changed before any lookup ever touched the week: there is
    // no earlier state to reconstruct.
    fixture
        .nfl
        .seed_for_test(&[], &changed_week_13_games())
        .await
        .unwrap();

    let got = fixture.nfl.slates_at(WEEK_13, WEDNESDAY).await.unwrap();
    let main = slate_named(&got, "Week 13");
    assert_eq!(
        ids(&fixture
            .nfl
            .slate_games_at(main.id, WEDNESDAY)
            .await
            .unwrap()),
        ["sun_2", "mnf"],
        "the first lookup captures Wednesday's selection"
    );
    assert_eq!(main.kickoff, Some(datetime!(2025-11-30 20:25 UTC)));
    assert!(
        store::slates::is_fixed(fixture.nfl.store.reader(), WEEK_13)
            .await
            .unwrap(),
        "the first lookup after the opening Monday fixes the week"
    );
    assert_eq!(
        store::slates::game_ids(fixture.nfl.store.reader(), main.id)
            .await
            .unwrap(),
        ["mnf", "sun_2"]
    );
}

/// A disappearing holiday keeps its stored identity but loses its stale
/// kickoff on every lookup — before Monday, on the first fixing lookup, and
/// after — so identical schedule data yields identical metadata each time.
#[tokio::test]
async fn disappearing_holiday_keeps_identity_and_clears_stale_kickoff() {
    let fixture = Fixture::connect().await;
    fixture
        .nfl
        .seed_for_test(&[], &week_16_games())
        .await
        .unwrap();

    // The tentative Christmas slate is established before the opening Monday.
    let established = fixture
        .nfl
        .slates_at(WEEK_16, BEFORE_WEEK_16)
        .await
        .unwrap();
    assert_eq!(
        names(&established),
        ["Christmas", "Week 16"],
        "known kickoffs ascending"
    );
    let christmas = slate_named(&established, "Christmas");
    assert_eq!(christmas.kickoff, Some(datetime!(2025-12-25 18:00 UTC)));
    let christmas_id = christmas.id;

    // The holiday's last game reschedules off December 25: the rule stops
    // proposing Christmas.
    fixture.replace_games(&moved_off_christmas_games()).await;

    // Game access already returns no Christmas members...
    assert!(
        fixture
            .nfl
            .slate_games_at(christmas_id, BEFORE_WEEK_16)
            .await
            .unwrap()
            .is_empty(),
        "the holiday game left the Christmas membership"
    );
    // ...but the metadata must not keep reporting the stale kickoff.
    let after_move = fixture
        .nfl
        .slates_at(WEEK_16, BEFORE_WEEK_16)
        .await
        .unwrap();
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

    // By-ID access agrees with the listing.
    let found = fixture
        .nfl
        .slate_at(christmas_id, BEFORE_WEEK_16)
        .await
        .unwrap()
        .expect("stored slate");
    assert_eq!(found.kickoff, None);

    // The first lookup at or after the opening Monday fixes the week and
    // reports the same metadata: no membership is inserted for the omitted
    // proposal, and the stale kickoff stays cleared.
    let first_fixed = fixture
        .nfl
        .slates_at(WEEK_16, WEEK_16_MONDAY)
        .await
        .unwrap();
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

    // Identical schedule data now yields identical metadata on every later
    // lookup.
    let later = fixture
        .nfl
        .slates_at(WEEK_16, XMAS_WEDNESDAY)
        .await
        .unwrap();
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
    let established = fixture.nfl.slates_at(WEEK_13, MONDAY).await.unwrap();
    let main_id = slate_named(&established, "Week 13").id;

    // After fixation: a selected game moves to Saturday and a new Sunday game
    // appears.
    fixture.replace_games(&rescheduled_after_fix_games()).await;

    let fixed_ids = store::slates::game_ids(fixture.nfl.store.reader(), main_id)
        .await
        .unwrap();
    assert_eq!(
        fixed_ids,
        ["sun_1", "sun_2"],
        "selected IDs remain unchanged"
    );
    let got = fixture.nfl.slate_games_at(main_id, THURSDAY).await.unwrap();
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
        fixture
            .nfl
            .slate_at(main_id, THURSDAY)
            .await
            .unwrap()
            .unwrap()
            .kickoff,
        Some(datetime!(2025-11-29 21:30 UTC)),
        "the stored kickoff refreshed from the fixed members"
    );
    assert_eq!(
        fixture
            .nfl
            .slate_at(main_id, THURSDAY)
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
    let established = fixture.nfl.slates_at(WEEK_13, MONDAY).await.unwrap();
    let main_id = slate_named(&established, "Week 13").id;

    fixture.replace_games(&changed_week_13_games()).await;

    let got = fixture.nfl.slate_games_at(main_id, BEFORE).await.unwrap();
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
        fixture
            .nfl
            .slate_at(unknown, MONDAY)
            .await
            .unwrap()
            .is_none(),
        "absent ids have no metadata"
    );
    assert!(
        fixture
            .nfl
            .slate_games_at(unknown, MONDAY)
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
