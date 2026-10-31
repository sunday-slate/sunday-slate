use std::str::FromStr;
use std::time::Duration;

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use tempfile::TempDir;
use time::OffsetDateTime;
use time::macros::datetime;

use nfl_data::slates;
use nfl_data::slates::{Slate, SlateId};
use nfl_data::{Game, NflData, NflDataConfig, Season, SeasonType, SeasonWeek, Week};

const WEEK_13: SeasonWeek = SeasonWeek {
    season: Season(2025),
    season_type: SeasonType::Reg,
    week: Week(13),
};

const WEEK_16: SeasonWeek = SeasonWeek {
    season: Season(2025),
    season_type: SeasonType::Reg,
    week: Week(16),
};

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

fn week_16_games() -> Vec<Game> {
    vec![game("wk16_1", 16, None)]
}

fn names(slates: &[Slate]) -> Vec<&str> {
    slates.iter().map(|slate| slate.name.as_str()).collect()
}

fn named<'a>(slates: &'a [Slate], name: &str) -> &'a Slate {
    slates
        .iter()
        .find(|slate| slate.name == name)
        .unwrap_or_else(|| panic!("no slate named {name} in {slates:?}"))
}

fn game_ids(games: &[Game]) -> Vec<String> {
    games.iter().map(|game| game.game_id.clone()).collect()
}

struct TempDatabases {
    config: NflDataConfig,
    provider_url: String,
    _dir: TempDir,
}

impl TempDatabases {
    async fn connect() -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let url = |name: &str| format!("sqlite:{}", dir.path().join(name).display());
        let config = NflDataConfig {
            nflverse_database_url: url("provider.db"),
            fanduel_database_url: url("fanduel.db"),
            database_url: url("store.db"),
            refresh_interval: None,
            season: Season(2025),
            ..NflDataConfig::default()
        };
        Self {
            config,
            provider_url: url("provider.db"),
            _dir: dir,
        }
    }

    async fn open(&self) -> NflData {
        NflData::connect(self.config.clone())
            .await
            .expect("nfl data")
    }

    async fn change_kickoff(&self, game_id: &str, kickoff: OffsetDateTime) {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::from_str(&self.provider_url)
                    .expect("valid provider url")
                    .journal_mode(SqliteJournalMode::Wal)
                    .busy_timeout(Duration::from_secs(10)),
            )
            .await
            .expect("independent provider connection");
        sqlx::query("UPDATE games SET kickoff = ? WHERE game_id = ?")
            .bind(kickoff)
            .bind(game_id)
            .execute(&pool)
            .await
            .expect("update cached kickoff");
        pool.close().await;
    }
}

#[tokio::test]
async fn public_slate_is_metadata_only() {
    let temp = TempDatabases::connect().await;
    let nfl = temp.open().await;
    let mut games = week_13_games();
    games.extend(week_16_games());
    nfl.seed_for_test(&[], &games).await.unwrap();
    nfl.refresh_slates().await.unwrap();

    let listed = slates::by_week(&nfl, WEEK_13).await.unwrap();
    assert_eq!(
        names(&listed),
        ["Thanksgiving", "Week 13"],
        "the holiday and main slates, known kickoffs ascending"
    );

    let Slate {
        id,
        season_week,
        name,
        kickoff,
    } = named(&listed, "Week 13").clone();
    assert_eq!(
        id,
        SlateId(20250131),
        "the derived main id for the seeded week"
    );
    assert_eq!(season_week, WEEK_13);
    assert_eq!(name, "Week 13");
    assert_eq!(kickoff, Some(datetime!(2025-11-30 17:00 UTC)));

    let thanksgiving = named(&listed, "Thanksgiving");
    assert_eq!(thanksgiving.id, SlateId(20250130), "the derived holiday id");
    assert_eq!(thanksgiving.season_week, WEEK_13);
    assert_eq!(thanksgiving.name, "Thanksgiving");
    assert_eq!(thanksgiving.kickoff, Some(datetime!(2025-11-27 17:30 UTC)));

    let unknown_kickoffs = slates::by_week(&nfl, WEEK_16).await.unwrap();
    assert_eq!(names(&unknown_kickoffs), ["Week 16"]);
    let week_16 = named(&unknown_kickoffs, "Week 16");
    assert_eq!(
        week_16.id,
        SlateId(20250161),
        "the derived main id for the unknown-kickoff week"
    );
    assert_eq!(week_16.kickoff, None, "the stored kickoff is nullable");
}

#[tokio::test]
async fn public_access_returns_metadata_and_games_separately() {
    let temp = TempDatabases::connect().await;
    let nfl = temp.open().await;
    nfl.seed_for_test(&[], &week_13_games()).await.unwrap();
    nfl.refresh_slates().await.unwrap();

    let listed = slates::by_week(&nfl, WEEK_13).await.unwrap();
    assert_eq!(names(&listed), ["Thanksgiving", "Week 13"]);

    for expected in &listed {
        assert_eq!(
            slates::by_id(&nfl, &expected.id).await.unwrap().as_ref(),
            Some(expected)
        );
    }

    let thanksgiving = named(&listed, "Thanksgiving");
    let main = named(&listed, "Week 13");
    assert_eq!(
        game_ids(&slates::games(&nfl, &thanksgiving.id).await.unwrap()),
        ["tg_1", "tg_2", "tg_3"]
    );
    assert_eq!(
        game_ids(&slates::games(&nfl, &main.id).await.unwrap()),
        ["sun_1", "sun_2"]
    );

    let unknown = SlateId(999_999);
    assert!(slates::by_id(&nfl, &unknown).await.unwrap().is_none());
    assert!(slates::games(&nfl, &unknown).await.unwrap().is_empty());
}

#[tokio::test]
async fn fixed_ids_and_metadata_survive_reopen() {
    let temp = TempDatabases::connect().await;
    let nfl = temp.open().await;
    nfl.seed_for_test(&[], &week_13_games()).await.unwrap();
    nfl.refresh_slates().await.unwrap();

    let listed = slates::by_week(&nfl, WEEK_13).await.unwrap();
    let main_id = named(&listed, "Week 13").id;
    let thanksgiving_id = named(&listed, "Thanksgiving").id;
    assert_eq!(main_id, SlateId(20250131), "the derived main id");
    assert_eq!(thanksgiving_id, SlateId(20250130), "the derived holiday id");
    let main_games = game_ids(&slates::games(&nfl, &main_id).await.unwrap());
    let thanksgiving_games = game_ids(&slates::games(&nfl, &thanksgiving_id).await.unwrap());
    drop(nfl);

    let nfl = temp.open().await;
    nfl.refresh_slates().await.unwrap();
    let reopened = slates::by_week(&nfl, WEEK_13).await.unwrap();
    assert_eq!(named(&reopened, "Week 13").id, main_id);
    assert_eq!(named(&reopened, "Thanksgiving").id, thanksgiving_id);
    assert_eq!(
        named(&reopened, "Thanksgiving").kickoff,
        Some(datetime!(2025-11-27 17:30 UTC))
    );
    assert_eq!(
        game_ids(&slates::games(&nfl, &main_id).await.unwrap()),
        main_games
    );
    assert_eq!(
        game_ids(&slates::games(&nfl, &thanksgiving_id).await.unwrap()),
        thanksgiving_games
    );
    drop(nfl);

    temp.change_kickoff("sun_1", datetime!(2025-11-29 21:30 UTC))
        .await;
    let nfl = temp.open().await;
    nfl.refresh_slates().await.unwrap();

    assert_eq!(
        game_ids(&slates::games(&nfl, &main_id).await.unwrap()),
        main_games
    );
    let games = slates::games(&nfl, &main_id).await.unwrap();
    let sun_1 = games
        .iter()
        .find(|game| game.game_id == "sun_1")
        .expect("the moved member");
    assert_eq!(sun_1.kickoff, Some(datetime!(2025-11-29 21:30 UTC)));

    let found = slates::by_id(&nfl, &main_id)
        .await
        .unwrap()
        .expect("stored slate");
    assert_eq!(found.id, main_id);
    assert_eq!(found.kickoff, Some(datetime!(2025-11-29 21:30 UTC)));
}

#[tokio::test]
async fn slate_access_has_no_salary_prerequisite() {
    let temp = TempDatabases::connect().await;
    let nfl = temp.open().await;
    nfl.seed_for_test(&[], &week_13_games()).await.unwrap();
    nfl.refresh_slates().await.unwrap();

    assert!(
        nfl.fanduel_weekly_summaries(&[]).await.unwrap().is_empty(),
        "no stored salary summaries exist before slate access"
    );

    let listed = slates::by_week(&nfl, WEEK_13).await.unwrap();
    let main_id = named(&listed, "Week 13").id;
    assert!(slates::by_id(&nfl, &main_id).await.unwrap().is_some());
    assert_eq!(
        game_ids(&slates::games(&nfl, &main_id).await.unwrap()),
        ["sun_1", "sun_2"]
    );

    assert!(
        nfl.fanduel_weekly_summaries(&[]).await.unwrap().is_empty(),
        "slate access persisted no salary summaries"
    );
}
