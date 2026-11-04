//! Snapshot build and update-worker tests.

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;
use sqlx::Connection;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use tempfile::TempDir;
use time::OffsetDateTime;
use time::macros::datetime;
use tokio::sync::mpsc;
use utils::background::RunnerState;

use super::updates::{PendingWork, UpdateInputs, run};
use super::{Snapshot, SnapshotSources};
use crate::NflDataError;
use crate::salaried_players::SalariedPlayer;
use crate::scoring::{score_defense, score_player};
use crate::slates::{self, SlateId};
use crate::store::player_link::{self, PlayerLink};
use crate::store::slates as slates_store;
use crate::{
    Game, NflData, NflDataConfig, Player, PlayerWeekStats, Season, SeasonType, SeasonWeek,
    TeamAbbr, TeamWeekStats, Week,
};
use fanduel_data::{
    Game as FdGame, Matchup, Player as FdPlayer, SalariedPlayer as FdSalariedPlayer,
};
use nfl_model::{DfsPosition, DfsPositionSlotType, NflPlayerId, PlayerOrDefenseId};
use nflverse_data::{AssetUpdate, Dataset, SyncEvent};

const SEASON: Season = Season(2026);
const PREVIOUS_SEASON: Season = Season(2025);
const SEASON_WEEK_5: SeasonWeek = SeasonWeek {
    season: SEASON,
    season_type: SeasonType::Reg,
    week: Week(5),
};
const PREVIOUS_WEEK_5: SeasonWeek = SeasonWeek {
    season: PREVIOUS_SEASON,
    season_type: SeasonType::Reg,
    week: Week(5),
};
/// Before the season's opening Monday, so seeded slates stay tentative.
const PINNED_NOW: OffsetDateTime = datetime!(2026-09-01 12:00 UTC);
/// A kickoff far past any plausible test clock, keeping the worker's slates
/// tentative.
const WORKER_WEEK_NUM: u8 = 14;
const WORKER_KICKOFF: OffsetDateTime = datetime!(2030-01-06 17:00 UTC);
/// The last week reconcile visits, in `SeasonWeek` order.
const LAST_WEEK: SeasonWeek = SeasonWeek {
    season: SEASON,
    season_type: SeasonType::Post,
    week: Week(21),
};
/// A Sunday kickoff in the past, inside the main slate's noon–6pm Eastern
/// window: every week of the fixed schedule stores it.
const PAST_KICKOFF: OffsetDateTime = datetime!(2026-01-04 17:00 UTC);

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
        season_type: SeasonType::Reg,
        week: Week(5),
    }
}

fn main_slate_id(season: Season) -> SlateId {
    SlateId(i64::from(week_5(season).encode()) * 10 + 1)
}

fn worker_week() -> SeasonWeek {
    SeasonWeek {
        season: SEASON,
        season_type: SeasonType::Reg,
        week: Week(WORKER_WEEK_NUM),
    }
}

fn worker_slate_id() -> SlateId {
    week_slate_id(worker_week())
}

/// The main slate id of one week.
fn week_slate_id(season_week: SeasonWeek) -> SlateId {
    SlateId(i64::from(season_week.encode()) * 10 + 1)
}

/// One past-dated game per week: `connect` fixes every week, so later
/// reconciles rewrite identical state and report no change.
fn past_dated_games() -> Vec<Game> {
    SeasonWeek::for_season(SEASON).map(past_game).collect()
}

fn past_game(season_week: SeasonWeek) -> Game {
    Game {
        game_id: format!("{}_{}_BUF_KC", season_week.season.0, season_week.week.0),
        season_week,
        kickoff: Some(PAST_KICKOFF),
        home_team: TeamAbbr::BUF,
        away_team: TeamAbbr::KC,
        home_score: None,
        away_score: None,
    }
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

fn worker_games() -> Vec<Game> {
    vec![Game {
        game_id: format!("2026_{WORKER_WEEK_NUM}_BUF_KC"),
        season_week: worker_week(),
        kickoff: Some(WORKER_KICKOFF),
        home_team: TeamAbbr::BUF,
        away_team: TeamAbbr::KC,
        home_score: None,
        away_score: None,
    }]
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
        season: SEASON,
        week: Week(week),
        season_type: SeasonType::Reg,
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

fn gamma_week_stats(week: u8) -> PlayerWeekStats {
    PlayerWeekStats {
        targets: 8,
        receptions: 8,
        receiving_yards: 120,
        ..player_stats("00-GAMMA", week)
    }
}

fn team_stats(team: TeamAbbr, opponent: TeamAbbr, week: u8) -> TeamWeekStats {
    TeamWeekStats {
        season: SEASON,
        week: Week(week),
        season_type: SeasonType::Reg,
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

/// The stats the snapshot averages: the week preceding the salaried one.
fn preceding_week_stats(week: u8) -> (Vec<PlayerWeekStats>, Vec<TeamWeekStats>) {
    (
        vec![mack_week_stats(week), gamma_week_stats(week)],
        vec![kc_week_stats(week), buf_week_stats(week)],
    )
}

/// The fixture's salary export with different salaries; re-saving updates the
/// same rows.
fn salaries_with(mack: u32, zack: u32, gamma: u32) -> String {
    SALARIES_CSV
        .replace("Runner,,0,9000", &format!("Runner,,0,{mack}"))
        .replace("Backer,,0,9000", &format!("Backer,,0,{zack}"))
        .replace("Catcher,,0,12000", &format!("Catcher,,0,{gamma}"))
}

/// Seeds provider players and both seasons' games, the week-5 salaries, the
/// player links, and the preceding week's stats, then stores both seasons'
/// slates at the pinned clock.
async fn seed_sources(nfl: &NflData) {
    nfl.seed_for_test(&seeded_players(), &seeded_games())
        .await
        .expect("seed provider");
    for (week, csv) in [
        (SEASON_WEEK_5, SALARIES_CSV),
        (PREVIOUS_WEEK_5, PREVIOUS_SEASON_SALARIES_CSV),
    ] {
        let rows = nfl
            .parse_fanduel_csv(week, csv.as_bytes())
            .expect("parse salaries");
        nfl.save_fanduel_salaries(&rows)
            .await
            .expect("save salaries");
    }
    player_link::save_links(&nfl.store, &seeded_links())
        .await
        .expect("save links");
    let (player_stats, team_stats) = preceding_week_stats(4);
    nfl.seed_week_stats_for_test(SEASON, &player_stats, &team_stats)
        .await
        .expect("seed week stats");
    for season in [SEASON, PREVIOUS_SEASON] {
        slates::populate_at(&nfl.provider, &nfl.store, season, PINNED_NOW)
            .await
            .expect("populate slates");
    }
}

async fn in_memory_fixture() -> NflData {
    let nfl = NflData::in_memory().await.expect("in-memory handle");
    seed_sources(&nfl).await;
    nfl
}

/// Seed the worker's sources from `games`; the slate tables stay empty so the
/// worker's first pass populates them.
async fn seed_worker_sources(nfl: &NflData, games: &[Game]) {
    nfl.seed_for_test(&seeded_players(), games)
        .await
        .expect("seed provider");
    let rows = nfl
        .parse_fanduel_csv(worker_week(), SALARIES_CSV.as_bytes())
        .expect("parse salaries");
    nfl.save_fanduel_salaries(&rows)
        .await
        .expect("save salaries");
    player_link::save_links(&nfl.store, &seeded_links())
        .await
        .expect("save links");
    let (player_stats, team_stats) = preceding_week_stats(WORKER_WEEK_NUM - 1);
    nfl.seed_week_stats_for_test(SEASON, &player_stats, &team_stats)
        .await
        .expect("seed week stats");
}

async fn in_memory_worker() -> NflData {
    let nfl = NflData::in_memory().await.expect("in-memory handle");
    seed_worker_sources(&nfl, &worker_games()).await;
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
                nflverse_database_url: url("provider.db"),
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

    fn provider_url(&self) -> String {
        format!("sqlite:{}", self._dir.path().join("provider.db").display())
    }
}

/// A file-backed handle seeded for the worker, plus the database URLs the
/// failure injections need.
async fn worker_handle() -> (NflData, TempDatabases) {
    let dbs = TempDatabases::new();
    let nfl = NflData::connect(dbs.config.clone()).await.expect("connect");
    seed_worker_sources(&nfl, &worker_games()).await;
    (nfl, dbs)
}

/// A file-backed handle whose whole schedule is dated in the past, so the
/// connecting reconcile fixes every week.
async fn fixed_worker_handle() -> (NflData, TempDatabases) {
    let dbs = TempDatabases::new();
    {
        let seeding = NflData::connect(dbs.config.clone()).await.expect("connect");
        seed_worker_sources(&seeding, &past_dated_games()).await;
    }
    let nfl = NflData::connect(dbs.config.clone())
        .await
        .expect("reconnect");
    assert!(
        slates_store::is_fixed(nfl.store.reader(), LAST_WEEK)
            .await
            .expect("is fixed"),
        "the past-dated schedule fixed the last week"
    );
    (nfl, dbs)
}

/// A raw connection to a database file the handle's pools do not own.
async fn raw_pool(url: &str) -> sqlx::SqlitePool {
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

/// Drop a table through a raw connection, breaking one step of the pipeline
/// deterministically while its other reads still work.
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

/// Recreate a previously dropped table, empty. The definition comes from
/// `sqlite_master`, never caller input.
async fn recreate_table(url: &str, definition: &str) {
    let pool = raw_pool(url).await;
    sqlx::raw_sql(sqlx::AssertSqlSafe(definition.to_owned()))
        .execute(&pool)
        .await
        .expect("recreate table");
    pool.close().await;
}

async fn count_rows(url: &str, sql: &'static str) -> i64 {
    let pool = raw_pool(url).await;
    let count: i64 = sqlx::query_scalar(sql)
        .fetch_one(&pool)
        .await
        .expect("count");
    pool.close().await;
    count
}

/// Fail every reconcile at one slate's kickoff update: reconciling that week
/// aborts after the earlier weeks committed.
async fn poison_slate_refresh(url: &str, slate_id: SlateId) {
    let pool = raw_pool(url).await;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "CREATE TRIGGER poison_slate BEFORE UPDATE ON slates WHEN NEW.id = {} \
         BEGIN SELECT RAISE(ABORT, 'poisoned reconcile'); END",
        slate_id.0
    )))
    .execute(&pool)
    .await
    .expect("poison trigger");
    pool.close().await;
}

async fn drop_poison_trigger(url: &str) {
    let pool = raw_pool(url).await;
    sqlx::query("DROP TRIGGER poison_slate")
        .execute(&pool)
        .await
        .expect("drop poison trigger");
    pool.close().await;
}

/// The worker's channels. The returned senders stay with the test, so
/// dropping them closes the worker's inputs for real.
fn worker_channels() -> (
    mpsc::UnboundedSender<SyncEvent>,
    mpsc::UnboundedSender<()>,
    UpdateInputs,
) {
    let (sync_tx, sync_rx) = mpsc::unbounded_channel();
    let (changes_tx, changes_rx) = mpsc::unbounded_channel();
    let inputs = UpdateInputs {
        sync_events: sync_rx,
        snapshot_changes: changes_rx,
    };
    (sync_tx, changes_tx, inputs)
}

fn default_published() -> Arc<ArcSwap<Snapshot>> {
    Arc::new(ArcSwap::from_pointee(Snapshot::default()))
}

fn spawn_worker(
    nfl: &NflData,
    published: Arc<ArcSwap<Snapshot>>,
    inputs: UpdateInputs,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(run(SnapshotSources::from(nfl), published, inputs))
}

fn sync_event(all_ok: bool, dataset: Dataset, season: Option<Season>) -> SyncEvent {
    SyncEvent {
        all_ok,
        updates: vec![AssetUpdate { dataset, season }],
    }
}

/// A sync that changed nothing: reconciliation is requested, rebuilding is not.
fn unchanged_sync_event() -> SyncEvent {
    SyncEvent {
        all_ok: true,
        updates: vec![],
    }
}

/// Poll until the publication satisfies `pred`, panicking with `what` after
/// five seconds.
async fn wait_for_publication(
    published: &Arc<ArcSwap<Snapshot>>,
    what: &str,
    pred: impl Fn(&Snapshot) -> bool,
) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if pred(&published.load_full()) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect(what);
}

/// Give a settled state a moment, so a worker that would run another pass has
/// the chance; used before asserting that nothing further happened.
async fn settle_passes() {
    tokio::time::sleep(Duration::from_millis(300)).await;
}

/// Watch `window`, failing the moment anything publishes. The caller has
/// parked or settled the pass, so surviving the window is a precondition.
async fn assert_publication_held(
    published: &Arc<ArcSwap<Snapshot>>,
    baseline: &Arc<Snapshot>,
    window: Duration,
    what: &str,
) {
    let watched = tokio::time::timeout(window, async {
        loop {
            assert!(
                Arc::ptr_eq(baseline, &published.load_full()),
                "{what}: a snapshot was published"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(
        watched.is_err(),
        "{what}: the watch ended before its window closed"
    );
}

/// Hold the store's WAL writer lock from an outside connection, so the
/// worker's next reconcile pauses at `BEGIN IMMEDIATE`. Dropping the guard
/// releases the lock.
async fn hold_store_writer(url: &str) -> sqlx::SqliteConnection {
    let mut conn = sqlx::SqliteConnection::connect_with(
        &SqliteConnectOptions::from_str(url)
            .expect("store url")
            .journal_mode(SqliteJournalMode::Wal),
    )
    .await
    .expect("raw connection");
    sqlx::query("BEGIN IMMEDIATE")
        .execute(&mut conn)
        .await
        .expect("lock the store writer");
    conn
}

/// Count reconcile passes: every tentative reconcile rewrites the week's
/// membership, so an insert trigger fires once per membership row per pass.
async fn install_pass_counter(url: &str) {
    let pool = raw_pool(url).await;
    sqlx::query("CREATE TABLE pass_log (n INTEGER)")
        .execute(&pool)
        .await
        .expect("log table");
    sqlx::query(
        "CREATE TRIGGER pass_count AFTER INSERT ON slate_games \
         BEGIN INSERT INTO pass_log VALUES (1); END",
    )
    .execute(&pool)
    .await
    .expect("count trigger");
    pool.close().await;
}

async fn reconcile_passes(url: &str) -> i64 {
    let firings = count_rows(url, "SELECT COUNT(*) FROM pass_log").await;
    let membership = count_rows(url, "SELECT COUNT(*) FROM slate_games").await;
    if membership == 0 {
        0
    } else {
        firings / membership
    }
}

async fn wait_for_passes(url: &str, passes: i64, what: &'static str) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if reconcile_passes(url).await >= passes {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect(what);
}

// ---- the snapshot build ----

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

    let snapshot = Snapshot::build_from_sources(&SnapshotSources::from(&nfl))
        .await
        .expect("build from sources");
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
    let zack = snapshot
        .player(&main_id, &NflPlayerId("00-ZACK".into()))
        .expect("zack is built");
    assert_eq!((zack.games_played, zack.fppg), (0, 0.0));

    let kc = snapshot.defense(&main_id, &TeamAbbr::KC).expect("kc dst");
    assert_eq!(kc.salary, 4800);
    assert_eq!(kc.fppg, score_defense(&kc_week_stats(4)).total);
    let buf = snapshot
        .salaried_player(&main_id, &PlayerOrDefenseId::Defense(TeamAbbr::BUF))
        .expect("buf dst");
    assert_eq!((buf.salary, buf.games_played, buf.fppg), (4900, 1, 0.0));
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
            nfl.provider.refresh_status(),
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
    drop_table(&dbs.provider_url(), "players").await;
    let Err(error) = NflData::connect(dbs.config.clone()).await else {
        panic!("build failure must be a connection error");
    };
    assert!(
        error.to_string().contains("players"),
        "the failure is the broken provider source: {error}"
    );
}

// ---- the ordered update worker ----

#[test]
fn pending_work_flags_classify_sync_events() {
    fn flags(all_ok: bool, dataset: Dataset, season: Option<Season>) -> (bool, bool) {
        let work = PendingWork::for_sync(&sync_event(all_ok, dataset, season), SEASON);
        (work.reconcile_slates, work.rebuild_snapshot)
    }

    // A committed relevant asset requests everything, even after a sibling
    // failure; a historical-only asset reconciles but never rebuilds.
    assert_eq!(
        flags(true, Dataset::PlayerWeekStats, Some(SEASON)),
        (true, true)
    );
    assert_eq!(
        flags(false, Dataset::PlayerWeekStats, Some(SEASON)),
        (true, true)
    );
    assert_eq!(
        flags(true, Dataset::PlayerWeekStats, Some(PREVIOUS_SEASON)),
        (true, false)
    );
    assert_eq!(
        flags(false, Dataset::PlayerWeekStats, Some(PREVIOUS_SEASON)),
        (false, false)
    );
    // Shared assets span every season.
    assert_eq!(flags(false, Dataset::Schedules, None), (true, true));
    assert_eq!(flags(false, Dataset::Players, None), (true, true));
    // An unchanged success reconciles only; a fully failed sync with no
    // commits requests nothing.
    let unchanged = PendingWork::for_sync(&unchanged_sync_event(), SEASON);
    assert_eq!(
        (unchanged.reconcile_slates, unchanged.rebuild_snapshot),
        (true, false)
    );
    let failed = PendingWork::for_sync(
        &SyncEvent {
            all_ok: false,
            updates: vec![],
        },
        SEASON,
    );
    assert_eq!(
        (failed.reconcile_slates, failed.rebuild_snapshot),
        (false, false)
    );
}

#[tokio::test]
async fn arrivals_during_a_pass_combine_into_one_followup_pass() {
    let (nfl, dbs) = worker_handle().await;
    install_pass_counter(&dbs.store_url()).await;
    let published = default_published();
    let (sync_tx, changes_tx, inputs) = worker_channels();

    // The startup pass pauses on the store writer; a burst arrives while it
    // runs and must reach one publication and one follow-up pass.
    let lock = hold_store_writer(&dbs.store_url()).await;
    let worker = spawn_worker(&nfl, Arc::clone(&published), inputs);
    let rows = nfl
        .parse_fanduel_csv(worker_week(), salaries_with(9000, 9400, 12000).as_bytes())
        .expect("parse salaries");
    nfl.save_fanduel_salaries(&rows)
        .await
        .expect("save salaries");
    sync_tx
        .send(sync_event(true, Dataset::PlayerWeekStats, Some(SEASON)))
        .expect("queued");
    sync_tx
        .send(sync_event(
            true,
            Dataset::PlayerWeekStats,
            Some(PREVIOUS_SEASON),
        ))
        .expect("queued");
    sync_tx
        .send(sync_event(false, Dataset::Schedules, None))
        .expect("queued");
    changes_tx.send(()).expect("queued");
    drop(lock);

    wait_for_publication(&published, "the released pass published", |snap| {
        snap.player(&worker_slate_id(), &NflPlayerId("00-ZACK".into()))
            .is_some_and(|zack| zack.salary == 9400)
    })
    .await;
    wait_for_passes(&dbs.store_url(), 2, "the follow-up pass reconciled").await;
    settle_passes().await;
    assert_eq!(
        reconcile_passes(&dbs.store_url()).await,
        2,
        "the burst ran one follow-up pass, not one per notification"
    );
    worker.abort();
}

#[tokio::test]
async fn the_worker_follows_its_open_input_and_old_readers_keep_their_snapshot() {
    let nfl = in_memory_worker().await;
    let published = default_published();
    let (sync_tx, changes_tx, inputs) = worker_channels();
    let worker = spawn_worker(&nfl, Arc::clone(&published), inputs);
    wait_for_publication(
        &published,
        "the startup pass built the seeded snapshot",
        |snap| snap.slate(&worker_slate_id()).is_some(),
    )
    .await;

    // A reader takes the published Arc; then the sync input closes and a
    // local invalidation still drives a rebuild.
    let old = published.load_full();
    assert_eq!(
        old.player(&worker_slate_id(), &NflPlayerId("00-MACK".into()))
            .expect("old mack")
            .salary,
        9000
    );
    drop(sync_tx);
    let rows = nfl
        .parse_fanduel_csv(worker_week(), salaries_with(9500, 9000, 12000).as_bytes())
        .expect("parse salaries");
    nfl.save_fanduel_salaries(&rows)
        .await
        .expect("save salaries");
    changes_tx.send(()).expect("invalidation queued");
    wait_for_publication(&published, "the rebuild published the new salary", |snap| {
        snap.player(&worker_slate_id(), &NflPlayerId("00-MACK".into()))
            .is_some_and(|mack| mack.salary == 9500)
    })
    .await;

    // The old reader keeps its snapshot; a new Arc was published.
    assert_eq!(
        old.player(&worker_slate_id(), &NflPlayerId("00-MACK".into()))
            .expect("old mack")
            .salary,
        9000,
        "the old reader's snapshot is untouched"
    );
    assert!(
        !Arc::ptr_eq(&old, &published.load_full()),
        "a new Arc was published"
    );

    // Both senders dropped: the worker exits on its own.
    drop(changes_tx);
    tokio::time::timeout(Duration::from_secs(3), worker)
        .await
        .expect("the worker exits once both inputs close")
        .expect("the worker task joins cleanly");
}

/// Re-save the worker week's salaries with Mack's salary changed.
async fn save_mack_salary(nfl: &NflData, salary: u32) {
    let rows = nfl
        .parse_fanduel_csv(worker_week(), salaries_with(salary, 9000, 12000).as_bytes())
        .expect("parse salaries");
    nfl.save_fanduel_salaries(&rows)
        .await
        .expect("save salaries");
}

/// The publication predicate for Mack at `salary`.
fn mack_at_salary(salary: u32) -> impl Fn(&Snapshot) -> bool {
    let slate_id = worker_slate_id();
    move |snap| {
        snap.player(&slate_id, &NflPlayerId("00-MACK".into()))
            .is_some_and(|mack| mack.salary == salary)
    }
}

#[tokio::test]
async fn a_failed_reconcile_retains_the_pending_rebuild() {
    let (nfl, dbs) = fixed_worker_handle().await;
    let published = default_published();
    let (sync_tx, _changes_tx, inputs) = worker_channels();
    let worker = spawn_worker(&nfl, Arc::clone(&published), inputs);

    // The startup pass settles the store and spends its own rebuild request.
    wait_for_publication(
        &published,
        "the startup pass published the seed salary",
        mack_at_salary(9000),
    )
    .await;
    let baseline = published.load_full();

    // The startup pass spent its own rebuild request and settled the store, so
    // from here a publication can only come from a failed pass's retained work.
    poison_slate_refresh(&dbs.store_url(), week_slate_id(LAST_WEEK)).await;
    save_mack_salary(&nfl, 9500).await;

    // The held store writer parks the pass before its first write, so the
    // bounded observation cannot race the fault: while the lock is held the
    // pass can neither reach the poisoned week nor publish.
    let lock = hold_store_writer(&dbs.store_url()).await;
    sync_tx.send(unchanged_sync_event()).expect("queued");
    assert_publication_held(
        &published,
        &baseline,
        Duration::from_secs(1),
        "the parked pass",
    )
    .await;
    drop(lock);

    // Released, the pass reconciles weeks 1..20 value-identically and fails at
    // the poisoned last week; the window gives it the runtime to do both.
    assert_publication_held(
        &published,
        &baseline,
        Duration::from_secs(1),
        "the poisoned reconcile",
    )
    .await;

    // Unchanged success on the settled state requests reconciliation only, so
    // the salary can publish only through the failed pass's retained rebuild.
    drop_poison_trigger(&dbs.store_url()).await;
    sync_tx.send(unchanged_sync_event()).expect("queued");
    wait_for_publication(
        &published,
        "the retained rebuild published the new salary",
        mack_at_salary(9500),
    )
    .await;
    worker.abort();
}

#[tokio::test]
async fn a_failed_build_retains_the_pending_rebuild() {
    let (nfl, dbs) = fixed_worker_handle().await;
    let published = default_published();
    let (sync_tx, _changes_tx, inputs) = worker_channels();
    let worker = spawn_worker(&nfl, Arc::clone(&published), inputs);

    // The startup pass settles the store and spends its own rebuild request.
    wait_for_publication(
        &published,
        "the startup pass published the seed salary",
        mack_at_salary(9000),
    )
    .await;
    let baseline = published.load_full();

    // Break the build input. This pass asks for its own rebuild, so the delta
    // waits until after the failure window: an escaped pass publishes the old
    // salary and the window catches it.
    let players_table = table_definition(&dbs.provider_url(), "players").await;
    drop_table(&dbs.provider_url(), "players").await;

    // The held store writer parks the pass before its first write, so nothing
    // can publish while the fault stands.
    let lock = hold_store_writer(&dbs.store_url()).await;
    sync_tx
        .send(sync_event(true, Dataset::PlayerWeekStats, Some(SEASON)))
        .expect("queued");
    assert_publication_held(
        &published,
        &baseline,
        Duration::from_secs(1),
        "the parked pass",
    )
    .await;
    drop(lock);

    // Released, the pass reconciles and then fails at the missing players read.
    assert_publication_held(
        &published,
        &baseline,
        Duration::from_secs(2),
        "the broken build",
    )
    .await;

    // Restore the input, seed the delta, and notify with an unchanged sync: the
    // reconciles report no change, so only the retained rebuild can publish.
    recreate_table(&dbs.provider_url(), &players_table).await;
    nfl.seed_for_test(&seeded_players(), &[])
        .await
        .expect("re-seed players");
    save_mack_salary(&nfl, 9500).await;
    sync_tx.send(unchanged_sync_event()).expect("queued");
    wait_for_publication(
        &published,
        "the retained rebuild published the new salary",
        mack_at_salary(9500),
    )
    .await;
    worker.abort();
}

// ---- the local save notification ----

/// A FanDuel salary row built directly: a player row unless `position` is
/// DST, on the fixture's KC@BUF matchup.
fn fd_salary(
    season_week: SeasonWeek,
    fd_id: u32,
    first_name: &str,
    last_name: &str,
    position: DfsPosition,
    team: TeamAbbr,
) -> FdSalariedPlayer {
    FdSalariedPlayer {
        player: (position != DfsPosition::Dst).then(|| FdPlayer {
            id: fd_id,
            first_name: first_name.to_owned(),
            last_name: last_name.to_owned(),
        }),
        game: FdGame {
            season_week,
            matchup: Matchup {
                away: TeamAbbr::KC,
                home: TeamAbbr::BUF,
            },
        },
        salary: 9000,
        position,
        team,
    }
}

/// Drain the invalidation channel while the worker has not taken its
/// receiver, so a test that never starts background work observes every
/// notification its saves produced.
fn drained_snapshot_changes(nfl: &NflData) -> usize {
    let mut guard = nfl
        .update_inputs
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let Some(inputs) = guard.as_mut() else {
        panic!("the update worker already took the inputs");
    };
    let mut count = 0;
    while inputs.snapshot_changes.try_recv().is_ok() {
        count += 1;
    }
    count
}

fn assert_notifications(nfl: &NflData, expected: usize, what: &str) {
    assert_eq!(drained_snapshot_changes(nfl), expected, "{what}");
}

#[tokio::test]
async fn saves_notify_only_after_committed_configured_season_work() {
    let dbs = TempDatabases::new();
    let nfl = NflData::connect(dbs.config.clone()).await.expect("connect");
    let mack = fd_salary(
        SEASON_WEEK_5,
        1,
        "Mack",
        "Runner",
        DfsPosition::Rb,
        TeamAbbr::KC,
    );
    let historical = fd_salary(
        PREVIOUS_WEEK_5,
        2,
        "Zack",
        "Back",
        DfsPosition::Rb,
        TeamAbbr::BUF,
    );

    // Empty saves change nothing.
    nfl.save_fanduel_salaries(&[])
        .await
        .expect("an empty salary save succeeds");
    nfl.save_player_links(&[])
        .await
        .expect("an empty link save succeeds");
    assert_notifications(&nfl, 0, "empty saves notify nothing");

    // Historical-only salary rows are not snapshot-visible.
    nfl.save_fanduel_salaries(std::slice::from_ref(&historical))
        .await
        .expect("historical save succeeds");
    assert_notifications(&nfl, 0, "a historical-only save notifies nothing");

    // A configured-season save notifies once; one save carrying both seasons
    // still notifies once.
    nfl.save_fanduel_salaries(std::slice::from_ref(&mack))
        .await
        .expect("current save succeeds");
    assert_notifications(&nfl, 1, "a configured-season save notifies once");
    nfl.save_fanduel_salaries(&[mack, historical])
        .await
        .expect("mixed save succeeds");
    assert_notifications(&nfl, 1, "one mixed save notifies once");
    assert_eq!(
        nfl.salaried_players_for(SEASON_WEEK_5)
            .await
            .expect("current rows")
            .len(),
        1,
        "the batch committed both weeks"
    );

    // An invalid batch rolls back without notifying.
    let mut broken = fd_salary(
        SEASON_WEEK_5,
        3,
        "Late",
        "Row",
        DfsPosition::Dst,
        TeamAbbr::BUF,
    );
    broken.player = Some(FdPlayer {
        id: 3,
        first_name: "Late".into(),
        last_name: "Row".into(),
    });
    let error = nfl
        .save_fanduel_salaries(&[broken])
        .await
        .expect_err("the invalid batch fails");
    assert!(
        matches!(error, NflDataError::Fanduel(_)),
        "the failure is the fanduel row validation: {error}"
    );
    assert_notifications(&nfl, 0, "a failed save notifies nothing");

    // A committed link save notifies once; a store that cannot write does not.
    let link = PlayerLink {
        fd_player_id: 1,
        gsis_player_id: "00-MACK".into(),
    };
    nfl.save_player_links(std::slice::from_ref(&link))
        .await
        .expect("link save succeeds");
    assert_notifications(&nfl, 1, "a nonempty link save notifies once");
    assert_eq!(
        player_link::links(nfl.store.reader()).await.expect("links"),
        HashMap::from([(1, "00-MACK".to_owned())])
    );
    drop_table(&dbs.store_url(), "player_links").await;
    assert!(
        nfl.save_player_links(&[link]).await.is_err(),
        "the broken store fails the save"
    );
    assert_notifications(&nfl, 0, "a rolled-back link save notifies nothing");
}
