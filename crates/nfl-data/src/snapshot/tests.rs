//! Snapshot build tests.

use std::str::FromStr;
use std::sync::Arc;

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions};
use tempfile::TempDir;
use time::OffsetDateTime;
use time::macros::datetime;
use utils::background::RunnerState;

use super::{SalariedPlayers, Snapshot};
use crate::salaried_players::{SalariedPlayer, SalariedPlayersBySlot};
use crate::scoring::{score_defense, score_player};
use crate::slates::{self, Slate, SlateId};
use crate::store::player_link::{self, PlayerLink};
use crate::{
    Game, NflData, NflDataConfig, Player, PlayerWeekStats, Season, SeasonWeek, TeamAbbr,
    TeamWeekStats, Week,
};
use nfl_model::{DfsPosition, DfsPositionSlotType, NflPlayerId, PlayerOrDefenseId};

const SEASON: Season = Season(2026);
const PREVIOUS_SEASON: Season = Season(2025);
const SEASON_WEEK_5: SeasonWeek = SeasonWeek {
    season: SEASON,
    week: Week(5),
};
const PREVIOUS_WEEK_5: SeasonWeek = SeasonWeek {
    season: PREVIOUS_SEASON,
    week: Week(5),
};
/// Before the season's opening Monday, so the seeded slates stay tentative.
const PINNED_NOW: OffsetDateTime = datetime!(2026-09-01 12:00 UTC);

const SALARIES_CSV: &str = "\
Id,Position,First Name,Nickname,Last Name,FPPG,Played,Salary,Game,Team,Opponent,Injury Indicator,Injury Details,Tier,,,Roster Position
9001-1,RB,Mack,Mack Runner,Runner,,0,9000,KC@BUF,KC,BUF,,,,,,RB/FLEX
9002-2,RB,Zack,Zack Back,Backer,,0,9000,KC@BUF,BUF,KC,,,,,,RB/FLEX
9003-3,WR,Gamma,Gamma Catch,Catcher,,0,12000,KC@BUF,KC,BUF,,,,,,WR/FLEX
9004-4,D,Buffalo,Buffalo Bills,Bills,,0,4900,KC@BUF,BUF,KC,,,,,,DEF
9005-5,D,Kansas,Kansas City Chiefs,Chiefs,,0,4800,KC@BUF,KC,BUF,,,,,,DEF
";

/// The previous season carries one resolvable DST salary: its slate would
/// qualify if the build ignored the configured season.
const PREVIOUS_SEASON_SALARIES_CSV: &str = "\
Id,Position,First Name,Nickname,Last Name,FPPG,Played,Salary,Game,Team,Opponent,Injury Indicator,Injury Details,Tier,,,Roster Position
9006-6,D,Kansas,Kansas City Chiefs,Chiefs,,0,4900,KC@BUF,KC,BUF,,,,,,DEF
";

fn week_5(season: Season) -> SeasonWeek {
    SeasonWeek {
        season,
        week: Week(5),
    }
}

fn main_slate_id(season: Season) -> SlateId {
    SlateId(i64::from(week_5(season).encode()) * 10 + 1)
}

fn seeded_games() -> Vec<Game> {
    vec![
        Game {
            game_id: "2026_05_BUF_KC".into(),
            season_week: SEASON_WEEK_5,
            kickoff: Some(datetime!(2026-10-04 17:00 UTC)),
            home_team: TeamAbbr::BUF,
            away_team: TeamAbbr::KC,
            home_score: None,
            away_score: None,
        },
        Game {
            game_id: "2025_05_BUF_KC".into(),
            season_week: PREVIOUS_WEEK_5,
            kickoff: Some(datetime!(2025-10-05 17:00 UTC)),
            home_team: TeamAbbr::BUF,
            away_team: TeamAbbr::KC,
            home_score: None,
            away_score: None,
        },
    ]
}

fn player(gsis_id: &str, name: &str, position: &str, team: TeamAbbr) -> Player {
    Player {
        gsis_id: gsis_id.into(),
        espn_id: None,
        full_name: name.into(),
        first_name: None,
        last_name: None,
        position: Some(position.into()),
        latest_team: Some(team),
        headshot_url: None,
    }
}

fn seeded_players() -> Vec<Player> {
    vec![
        player("00-MACK", "Mack Runner", "RB", TeamAbbr::KC),
        player("00-ZACK", "Zack Back", "RB", TeamAbbr::BUF),
        player("00-GAMMA", "Gamma Catch", "WR", TeamAbbr::KC),
    ]
}

fn seeded_links() -> Vec<PlayerLink> {
    vec![
        PlayerLink {
            fd_player_id: 1,
            gsis_player_id: "00-MACK".into(),
        },
        PlayerLink {
            fd_player_id: 2,
            gsis_player_id: "00-ZACK".into(),
        },
        PlayerLink {
            fd_player_id: 3,
            gsis_player_id: "00-GAMMA".into(),
        },
    ]
}

fn player_stats(gsis_id: &str, week: u8) -> PlayerWeekStats {
    PlayerWeekStats {
        season_week: SeasonWeek::new(SEASON, Week(week)).unwrap(),
        gsis_id: gsis_id.into(),
        team: TeamAbbr::KC,
        opponent: Some(TeamAbbr::BUF),
        completions: 0,
        attempts: 0,
        passing_yards: 0,
        passing_tds: 0,
        passing_interceptions: 0,
        rushing_attempts: 0,
        rushing_yards: 0,
        rushing_tds: 0,
        targets: 0,
        receptions: 0,
        receiving_yards: 0,
        receiving_tds: 0,
        fumbles_lost: 0,
        two_point_conversions: 0,
        special_teams_tds: 0,
        fumble_recovery_tds: 0,
    }
}

fn mack_week_stats(week: u8) -> PlayerWeekStats {
    PlayerWeekStats {
        rushing_attempts: 1,
        rushing_yards: 150,
        rushing_tds: 1,
        ..player_stats("00-MACK", week)
    }
}

fn team_stats(team: TeamAbbr, opponent: TeamAbbr, week: u8) -> TeamWeekStats {
    TeamWeekStats {
        season_week: SeasonWeek::new(SEASON, Week(week)).unwrap(),
        team,
        opponent,
        game_id: format!("2026_{week:02}_BUF_KC"),
        sacks: 0,
        interceptions: 0,
        fumble_recoveries: 0,
        safeties: 0,
        touchdowns: 0,
        blocked_kicks: 0,
        conversion_returns: 0,
        points_allowed: 0,
    }
}

fn kc_week_stats(week: u8) -> TeamWeekStats {
    TeamWeekStats {
        sacks: 3,
        interceptions: 1,
        points_allowed: 10,
        ..team_stats(TeamAbbr::KC, TeamAbbr::BUF, week)
    }
}

fn buf_week_stats(week: u8) -> TeamWeekStats {
    TeamWeekStats {
        points_allowed: 24,
        ..team_stats(TeamAbbr::BUF, TeamAbbr::KC, week)
    }
}

/// Seeds nflverse players and both seasons' games, the week-5 salaries, the
/// player links, and the preceding week's stats, then stores both seasons'
/// slates at the pinned clock.
async fn seed_sources(nfl: &NflData) {
    nfl.seed_for_test(&seeded_players(), &seeded_games())
        .await
        .expect("seed nflverse");
    for (week, csv) in [
        (SEASON_WEEK_5, SALARIES_CSV),
        (PREVIOUS_WEEK_5, PREVIOUS_SEASON_SALARIES_CSV),
    ] {
        let rows = crate::salaries::parse_fanduel_csv(&nfl.fanduel, week, csv.as_bytes())
            .expect("parse salaries");
        crate::salaries::save_fanduel_salaries(&nfl.fanduel, &rows)
            .await
            .expect("save salaries");
    }
    player_link::save_links(&nfl.store, &seeded_links())
        .await
        .expect("save links");
    nfl.seed_week_stats_for_test(
        SEASON,
        &[mack_week_stats(4)],
        &[kc_week_stats(4), buf_week_stats(4)],
    )
    .await
    .expect("seed week stats");
    for season in [SEASON, PREVIOUS_SEASON] {
        slates::populate_at(&nfl.nflverse, &nfl.store, season, PINNED_NOW)
            .await
            .expect("populate slates");
    }
}

async fn in_memory_fixture() -> NflData {
    let nfl = NflData::in_memory().await.expect("in-memory handle");
    seed_sources(&nfl).await;
    nfl
}

fn names(players: &[SalariedPlayer]) -> Vec<&str> {
    players.iter().map(|player| player.name.as_str()).collect()
}

/// Temporary file databases; the directory outlives every connection opened
/// against it.
struct TempDatabases {
    config: NflDataConfig,
    _dir: TempDir,
}

impl TempDatabases {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let url = |name: &str| format!("sqlite:{}", dir.path().join(name).display());
        TempDatabases {
            config: NflDataConfig {
                nflverse_database_url: url("nflverse.db"),
                fanduel_database_url: url("fanduel.db"),
                database_url: url("store.db"),
                refresh_interval: None,
                season: SEASON,
                ..NflDataConfig::default()
            },
            _dir: dir,
        }
    }

    fn store_url(&self) -> String {
        format!("sqlite:{}", self._dir.path().join("store.db").display())
    }

    fn nflverse_url(&self) -> String {
        format!("sqlite:{}", self._dir.path().join("nflverse.db").display())
    }
}

/// A raw connection to a database file the handle's pools do not own.
async fn raw_pool(url: &str) -> SqlitePool {
    SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::from_str(url)
                .expect("valid url")
                .journal_mode(SqliteJournalMode::Wal),
        )
        .await
        .expect("raw connection")
}

async fn drop_table(url: &str, table: &'static str) {
    let pool = raw_pool(url).await;
    sqlx::query(sqlx::AssertSqlSafe(format!("DROP TABLE {table}")))
        .execute(&pool)
        .await
        .expect("drop table");
    pool.close().await;
}

async fn table_definition(url: &str, table: &'static str) -> String {
    let pool = raw_pool(url).await;
    let sql: String =
        sqlx::query_scalar("SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?")
            .bind(table)
            .fetch_one(&pool)
            .await
            .expect("table definition");
    pool.close().await;
    sql
}

async fn recreate_table(url: &str, definition: &str) {
    let pool = raw_pool(url).await;
    sqlx::raw_sql(sqlx::AssertSqlSafe(definition.to_owned()))
        .execute(&pool)
        .await
        .expect("recreate table");
    pool.close().await;
}

#[tokio::test]
async fn build_uses_the_configured_seasons_salaried_slates() {
    let nfl = in_memory_fixture().await;
    let previous_id = main_slate_id(PREVIOUS_SEASON);
    // the 2025 slate is stored with a resolvable salary; only the configured
    // season is built
    assert!(
        slates::by_id(&nfl, &previous_id)
            .await
            .expect("by id")
            .is_some()
    );

    let snapshot = Snapshot::build(&nfl).await.expect("build");
    let main_id = main_slate_id(SEASON);

    assert_eq!(
        snapshot.slates().map(|slate| slate.id).collect::<Vec<_>>(),
        [main_id]
    );
    assert!(
        snapshot.slate(&previous_id).is_none(),
        "the 2025 slate is outside the configured season"
    );
    let slate = snapshot.slate(&main_id).expect("week 5 slate");
    assert_eq!(slate.name, "Week 5");
    assert_eq!(slate.kickoff, Some(datetime!(2026-10-04 17:00 UTC)));

    // Buckets sort by salary descending, then name.
    let rb = snapshot
        .salaried_players_for_position(&main_id, &DfsPositionSlotType::Position(DfsPosition::Rb));
    assert_eq!(names(rb), ["Mack Runner", "Zack Back"]);
    let flex = snapshot.salaried_players_for_position(&main_id, &DfsPositionSlotType::Flex);
    assert_eq!(names(flex), ["Gamma Catch", "Mack Runner", "Zack Back"]);
    let dst = snapshot
        .salaried_players_for_position(&main_id, &DfsPositionSlotType::Position(DfsPosition::Dst));
    assert_eq!(names(dst), ["BUF DST", "KC DST"]);

    // Lookups carry salary, the resolved game, and the preceding averages.
    let mack = snapshot
        .player(&main_id, &NflPlayerId("00-MACK".into()))
        .expect("mack is built");
    assert_eq!((mack.salary, mack.games_played), (9000, 1));
    assert_eq!(mack.fppg, score_player(&mack_week_stats(4)).total);
    assert_eq!(mack.game.game_id, "2026_05_BUF_KC");
    assert_eq!(
        (mack.game.home_team, mack.game.away_team),
        (TeamAbbr::BUF, TeamAbbr::KC)
    );
    let kc = snapshot.defense(&main_id, &TeamAbbr::KC).expect("kc dst");
    assert_eq!(kc.salary, 4800);
    assert_eq!(kc.fppg, score_defense(&kc_week_stats(4)).total);
    let buf = snapshot
        .salaried_player(&main_id, &PlayerOrDefenseId::Defense(TeamAbbr::BUF))
        .expect("buf dst");
    assert_eq!((buf.salary, buf.games_played, buf.fppg), (4900, 1, 0.0));
}

#[tokio::test]
async fn games_for_slate_does_not_require_salaries() {
    let nfl = NflData::in_memory().await.expect("in-memory handle");
    // Games only: no players, links, or salaries are seeded.
    nfl.seed_for_test(&[], &seeded_games())
        .await
        .expect("seed games");
    slates::populate_at(&nfl.nflverse, &nfl.store, SEASON, PINNED_NOW)
        .await
        .expect("populate slates");

    nfl.rebuild_snapshot().await.expect("rebuild");
    let snapshot = nfl.snapshot();
    drop(nfl);

    let main_id = main_slate_id(SEASON);
    assert!(
        snapshot.slate(&main_id).is_none(),
        "without salaried players the slate is not exposed"
    );
    let games = snapshot.games_for_slate(&main_id);
    assert_eq!(games.len(), 1, "the stored membership resolves one game");
    let game = &games[0];
    assert_eq!(game.game_id, "2026_05_BUF_KC");
    assert_eq!(game.kickoff, Some(datetime!(2026-10-04 17:00 UTC)));
    assert_eq!(
        (game.home_team, game.away_team),
        (TeamAbbr::BUF, TeamAbbr::KC)
    );
}

#[tokio::test]
async fn games_for_slate_is_empty_for_absent_slates() {
    let nfl = in_memory_fixture().await;
    let snapshot = Snapshot::build(&nfl).await.expect("build");

    assert!(
        snapshot.games_for_slate(&main_slate_id(SEASON)).len() == 1,
        "the configured season's slate carries its game"
    );
    assert!(
        snapshot.games_for_slate(&SlateId(-1)).is_empty(),
        "an unknown id has no games"
    );
    assert!(
        snapshot
            .games_for_slate(&main_slate_id(PREVIOUS_SEASON))
            .is_empty(),
        "a stored slate outside the configured season has no games"
    );
}

#[tokio::test]
async fn connect_populates_slates_before_the_initial_snapshot() {
    let dbs = TempDatabases::new();

    let main_id = {
        let nfl = NflData::connect(dbs.config.clone())
            .await
            .expect("first connect");
        seed_sources(&nfl).await;
        let main_id = main_slate_id(SEASON);

        // Empty the slate tables: the next connect must repopulate before it
        // can build a snapshot containing this slate.
        nfl.store
            .write_tx(async |conn| {
                sqlx::query("DELETE FROM slate_games")
                    .execute(&mut *conn)
                    .await?;
                sqlx::query("DELETE FROM slates")
                    .execute(&mut *conn)
                    .await?;
                Ok::<(), sqlx::Error>(())
            })
            .await
            .expect("clear slate rows");
        assert!(
            slates::by_id(&nfl, &main_id)
                .await
                .expect("by id")
                .is_none(),
            "the slate rows are gone"
        );
        main_id
    };

    let nfl = NflData::connect(dbs.config.clone())
        .await
        .expect("reconnect");
    let snapshot = nfl.snapshot();
    assert!(
        snapshot.slate(&main_id).is_some(),
        "connect populated the slates before the initial build"
    );
    assert!(
        snapshot
            .player(&main_id, &NflPlayerId("00-MACK".into()))
            .is_some(),
        "the initial build resolved the stored salaries and links"
    );
    assert!(
        matches!(
            nfl.nflverse.refresh_status(),
            RunnerState::Idle { last: None }
        ),
        "construction started no background refresh"
    );
}

#[tokio::test]
async fn connect_propagates_population_and_build_errors() {
    // Population fails on the missing membership table.
    let dbs = TempDatabases::new();
    drop(NflData::connect(dbs.config.clone()).await.expect("connect"));
    drop_table(&dbs.store_url(), "slate_games").await;
    let Err(error) = NflData::connect(dbs.config.clone()).await else {
        panic!("population failure must be a connection error");
    };
    assert!(
        error.to_string().contains("slate_games"),
        "the failure is the broken slate store: {error}"
    );

    // The build fails on the missing players table.
    let dbs = TempDatabases::new();
    {
        let nfl = NflData::connect(dbs.config.clone()).await.expect("connect");
        seed_sources(&nfl).await;
    }
    drop_table(&dbs.nflverse_url(), "players").await;
    let Err(error) = NflData::connect(dbs.config.clone()).await else {
        panic!("build failure must be a connection error");
    };
    assert!(
        error.to_string().contains("players"),
        "the failure is the broken nflverse source: {error}"
    );
}

#[tokio::test]
async fn a_failed_rebuild_keeps_the_published_snapshot() {
    let dbs = TempDatabases::new();
    let nfl = NflData::connect(dbs.config.clone()).await.expect("connect");
    seed_sources(&nfl).await;
    nfl.rebuild_snapshot().await.expect("rebuild the seed");
    let published = nfl.snapshot();
    let main_id = main_slate_id(SEASON);
    assert!(
        published
            .player(&main_id, &NflPlayerId("00-MACK".into()))
            .is_some()
    );

    // Break a source the build reads: the rebuild fails and readers keep the
    // snapshot they already hold.
    let definition = table_definition(&dbs.nflverse_url(), "players").await;
    drop_table(&dbs.nflverse_url(), "players").await;
    assert!(nfl.rebuild_snapshot().await.is_err());
    assert!(
        Arc::ptr_eq(&published, &nfl.snapshot()),
        "the failed build left the publication alone"
    );

    // Repairing the source lets the next request publish again.
    recreate_table(&dbs.nflverse_url(), &definition).await;
    nfl.rebuild_snapshot().await.expect("the next build");
    assert!(!Arc::ptr_eq(&published, &nfl.snapshot()));
}

#[tokio::test]
async fn published_snapshot_carries_multiday_and_eastern_status() {
    let nfl = NflData::in_memory().await.expect("in-memory handle");
    let week = SeasonWeek {
        season: SEASON,
        week: Week(19),
    };
    let games = vec![
        Game {
            game_id: "2026_19_BUF_KC".into(),
            season_week: week,
            kickoff: Some(datetime!(2027-01-09 21:00 UTC)),
            home_team: TeamAbbr::KC,
            away_team: TeamAbbr::BUF,
            home_score: None,
            away_score: None,
        },
        Game {
            game_id: "2026_19_GB_DET".into(),
            season_week: week,
            kickoff: Some(datetime!(2027-01-10 18:00 UTC)),
            home_team: TeamAbbr::DET,
            away_team: TeamAbbr::GB,
            home_score: None,
            away_score: None,
        },
    ];
    nfl.seed_for_test(&[], &games).await.expect("seed games");
    let csv = "\
Id,Position,First Name,Nickname,Last Name,FPPG,Played,Salary,Game,Team,Opponent,Injury Indicator,Injury Details,Tier,,,Roster Position
9101-1,D,Kansas,Kansas City Chiefs,Chiefs,,0,4800,BUF@KC,KC,BUF,,,,,,DEF
9102-2,D,Detroit,Detroit Lions,Lions,,0,4700,GB@DET,DET,GB,,,,,,DEF
";
    let rows = crate::salaries::parse_fanduel_csv(&nfl.fanduel, week, csv.as_bytes())
        .expect("parse salaries");
    crate::salaries::save_fanduel_salaries(&nfl.fanduel, &rows)
        .await
        .expect("save salaries");
    slates::populate_at(
        &nfl.nflverse,
        &nfl.store,
        SEASON,
        datetime!(2027-01-01 12:00 UTC),
    )
    .await
    .expect("populate slates");
    nfl.rebuild_snapshot().await.expect("rebuild");

    let main_id = SlateId(i64::from(week.encode()) * 10 + 1);
    let snapshot = nfl.snapshot();
    let slate = snapshot.slate(&main_id).expect("the salaried week's slate");
    assert_eq!(slate.kickoff, Some(datetime!(2027-01-09 21:00 UTC)));
    assert!(slate.multiday, "Saturday and Sunday span two Eastern dates");
    assert!(
        snapshot.slates().any(|candidate| candidate.id == main_id),
        "the published slate set resolves the same id"
    );
    assert_eq!(
        snapshot.current_slate().map(|candidate| candidate.id),
        Some(main_id),
        "the multiday slate is the current one"
    );

    assert!(slate.upcoming(datetime!(2027-01-09 20:00 UTC)));
    assert!(!slate.locked(datetime!(2027-01-09 20:00 UTC)));
    assert!(slate.live(datetime!(2027-01-10 20:00 UTC)));
    assert!(!slate.finished(datetime!(2027-01-10 20:00 UTC)));
    assert!(slate.finished(datetime!(2027-01-11 05:00 UTC)));
    assert!(!slate.live(datetime!(2027-01-11 05:00 UTC)));
}

fn accessor_slate(id: i64, name: &str) -> Slate {
    Slate {
        id: SlateId(id),
        season_week: SEASON_WEEK_5,
        name: name.into(),
        kickoff: Some(datetime!(2026-10-04 17:00 UTC)),
        multiday: false,
    }
}

fn salaried(
    nfl_player_id: Option<NflPlayerId>,
    name: &str,
    position: DfsPosition,
    team: TeamAbbr,
    salary: u32,
) -> SalariedPlayer {
    SalariedPlayer {
        id: nfl_player_id,
        game: Arc::new(Game {
            game_id: "2026_05_BUF_KC".into(),
            season_week: SEASON_WEEK_5,
            kickoff: Some(datetime!(2026-10-04 17:00 UTC)),
            home_team: TeamAbbr::BUF,
            away_team: TeamAbbr::KC,
            home_score: None,
            away_score: None,
        }),
        name: name.into(),
        position,
        team,
        salary,
        games_played: 0,
        fppg: 0.0,
    }
}

#[test]
fn current_slate_is_the_greatest_populated_id() {
    assert!(
        Snapshot::default().current_slate().is_none(),
        "an empty snapshot has no current slate"
    );

    let later = accessor_slate(2, "Week 2");
    let mut snapshot = Snapshot::default();
    snapshot
        .slates
        .insert(SlateId(1), accessor_slate(1, "Week 1"));
    snapshot.slates.insert(later.id, later.clone());

    assert_eq!(snapshot.current_slate(), Some(&later));
}

#[test]
fn players_for_slate_is_player_only_and_position_independent() {
    let slate_id = SlateId(1);
    let mack = salaried(
        Some(NflPlayerId("00-MACK".into())),
        "Mack Runner",
        DfsPosition::Rb,
        TeamAbbr::KC,
        9000,
    );
    let gamma = salaried(
        Some(NflPlayerId("00-GAMMA".into())),
        "Gamma Catch",
        DfsPosition::Wr,
        TeamAbbr::KC,
        12000,
    );
    let kc_dst = salaried(None, "KC DST", DfsPosition::Dst, TeamAbbr::KC, 4800);

    let mut by_slot = SalariedPlayersBySlot::new();
    by_slot.insert(
        DfsPositionSlotType::Position(DfsPosition::Rb),
        vec![mack.clone()],
    );
    by_slot.insert(
        DfsPositionSlotType::Position(DfsPosition::Wr),
        vec![gamma.clone()],
    );
    by_slot.insert(DfsPositionSlotType::Flex, vec![gamma.clone(), mack.clone()]);
    by_slot.insert(
        DfsPositionSlotType::Position(DfsPosition::Dst),
        vec![kc_dst],
    );
    let mut snapshot = Snapshot::default();
    snapshot
        .slates
        .insert(slate_id, accessor_slate(slate_id.0, "Week 5"));
    snapshot
        .salaried_players
        .insert(slate_id, SalariedPlayers::new(by_slot));

    let players = snapshot
        .players_for_slate(&slate_id)
        .expect("populated slate");
    let mut ids: Vec<&str> = players.keys().map(NflPlayerId::as_ref).collect();
    ids.sort_unstable();
    assert_eq!(ids, ["00-GAMMA", "00-MACK"], "no flex duplicates, no DST");
    assert_eq!(players[&NflPlayerId("00-MACK".into())].salary, 9000);
    assert_eq!(players[&NflPlayerId("00-MACK".into())].name, "Mack Runner");
    assert_eq!(players[&NflPlayerId("00-GAMMA".into())].salary, 12000);
    assert_eq!(players[&NflPlayerId("00-GAMMA".into())].name, "Gamma Catch");

    assert!(
        snapshot.players_for_slate(&SlateId(99)).is_none(),
        "an unknown slate has no player map"
    );
}
