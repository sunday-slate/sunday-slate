use crate::{avatars::AvatarFetchRunner, live::LiveContestHub};
use crate::{router, sessions, tests::utils};
use axum::Router;
use axum_test::multipart::{MultipartForm, Part};
use nfl_data::{
    AdminConfig, Game, NflData, Player, Season, SeasonWeek, TeamAbbr, Week, slates::SlateId,
};
use std::sync::Arc;
use tempfile::TempDir;
use time::{Duration, OffsetDateTime, macros::datetime};

use axum::http::StatusCode;
use axum_test::{TestResponse, TestServer};
use nfl_model::NflPlayerId;
use sqlx::SqlitePool;

use crate::mail::Mailer;
use crate::tests::factories::{self, GeneratedUser, UserOptions};
use crate::{AppState, Config, Db, User};

pub struct TestApp {
    pub server: TestServer,
    pub pool: SqlitePool,
    pub mailer: Mailer,
    pub nfl: Arc<NflData>,
    pub state: AppState,
    // Keeps the per-test media directory alive; removed on drop.
    pub _media_dir: TempDir,
}

impl TestApp {
    /// Build a `TestApp` from a pool created by `#[sqlx::test]`.
    /// Uses `Mailer::capture` for the mailer.
    pub async fn from_pool(pool: SqlitePool) -> Self {
        Self::from_pool_with_mailer(pool, Mailer::capture("Sunday Slate <no-reply@example.com>"))
            .await
    }

    /// Like `from_pool` but accepts a custom `Mailer` (e.g. `failing_mailer()`).
    pub async fn from_pool_with_mailer(pool: SqlitePool, mailer: Mailer) -> Self {
        Self::build(pool, mailer, None, false, None).await
    }

    /// Like `from_pool_at`, with an explicit NFL-data handle (e.g. one whose
    /// season differs from the in-memory default).
    pub async fn from_pool_with_nfl_at(
        pool: SqlitePool,
        nfl: Arc<NflData>,
        now: OffsetDateTime,
    ) -> Self {
        Self::build(
            pool,
            Mailer::capture("Sunday Slate <no-reply@example.com>"),
            Some(now),
            false,
            Some(nfl),
        )
        .await
    }

    async fn build(
        pool: SqlitePool,
        mailer: Mailer,
        now_override: Option<OffsetDateTime>,
        live_dev_feed: bool,
        nfl_override: Option<Arc<NflData>>,
    ) -> Self {
        let media_dir = tempfile::tempdir().expect("temp media dir");

        let nfl = match nfl_override {
            Some(nfl) => nfl,
            None => Arc::new(NflData::in_memory().await.expect("in-memory nfl-data")),
        };

        let config = Config {
            database_url: "sqlite://:memory:".to_string(),
            bind_addr: "127.0.0.1:0".to_string(),
            base_url: "http://localhost:3000".to_string(),
            mail_from: "Sunday Slate <no-reply@example.com>".to_string(),
            nflverse_github_token: None,
            tank01_api_key: None,
            live_dev_feed,
            live_dev_feed_tick_ms: 3000,
            nflverse_sync_interval_secs: 0,
            nflverse_database_url: "sqlite://./storage/nflverse-cache.db".to_string(),
            fanduel_database_url: "sqlite://:memory:".to_string(),
            nfl_data_database_url: "sqlite://:memory:".to_string(),
            season: nfl.season().0,
            media_dir: media_dir.path().to_path_buf(),
            smtp: None,
            now_override,
        };

        let (session_layer, sessions) = sessions::layer_in_memory()
            .await
            .expect("build session layer");

        let db = Db::test(pool.clone());

        let avatar_runner = Arc::new(AvatarFetchRunner::new());

        let state = AppState {
            db,
            sessions,
            config: Arc::new(config),
            mailer: mailer.clone(),
            nfl: nfl.clone(),
            avatar_runner,
            live: Arc::new(LiveContestHub::new(true)),
        };

        // Cookie jar on: each response's Set-Cookie is saved and replayed on the
        // next request from this server, so login/flash/logout carry automatically.
        let server = TestServer::builder()
            .save_cookies()
            .build(router::build(state.clone(), session_layer));

        TestApp {
            server,
            pool,
            mailer,
            nfl,
            state,
            _media_dir: media_dir,
        }
    }

    pub fn state(&self) -> AppState {
        self.state.clone()
    }

    /// Build a `TestApp` with its own fresh, migrated in-memory SQLite
    /// database instead of one supplied by `#[sqlx::test]`. Use this for
    /// tests that don't need to seed data before the app exists — seed via
    /// `&app.pool` afterward if you do. Uses `Mailer::capture`.
    pub async fn new() -> Self {
        Self::from_pool(utils::in_memory_pool().await).await
    }

    /// Build a host test app around a mock-configured NFL-data instance.
    pub async fn new_with_nfl(nfl: Arc<NflData>) -> Self {
        Self::build(
            utils::in_memory_pool().await,
            Mailer::capture("Sunday Slate <no-reply@example.com>"),
            None,
            false,
            Some(nfl),
        )
        .await
    }

    /// Like `new` but accepts a custom `Mailer` (e.g. `failing_mailer()`).
    pub async fn new_with_mailer(mailer: Mailer) -> Self {
        Self::from_pool_with_mailer(utils::in_memory_pool().await, mailer).await
    }

    /// Log in as `user` via the test-only `/__test/login/{user_id}` route —
    /// no password needed, works with any `User` in the database. The cookie
    /// jar saves the session cookie, so subsequent requests from this app are
    /// authenticated. Returns the raw `Set-Cookie` header value for the few
    /// tests that assert cookie attributes.
    /// Create a site admin and sign in as them.
    pub async fn login_admin(&self) -> GeneratedUser {
        let admin = factories::user(
            &self.pool,
            UserOptions {
                is_admin: true,
                ..Default::default()
            },
        )
        .await;
        self.login_as(&admin.user).await;
        admin
    }

    pub async fn login_as(&self, user: &User) -> String {
        let resp = self
            .server
            .post(&format!("/__test/login/{}", user.id))
            .await;

        resp.header("set-cookie").to_str().unwrap().to_string()
    }

    pub async fn login(&self, gen_user: &GeneratedUser) -> String {
        let resp = self
            .server
            .post("/login")
            .form(&[
                ("email", &gen_user.user.email),
                ("password", &gen_user.password),
            ])
            .await;

        resp.header("set-cookie").to_str().unwrap().to_string()
    }

    /// Drop every cookie from the jar, so subsequent requests are anonymous.
    /// Used by journey tests that switch identities mid-flow (e.g. a
    /// commissioner sends an invite, then a fresh user accepts it).
    pub fn clear_cookies(&mut self) {
        self.server.clear_cookies();
    }

    /// GET `/` and follow the dispatcher's redirect to the page for the
    /// current state. Returns the final OK response.
    pub async fn home(&self) -> TestResponse {
        let resp = self.get("/").await;
        resp.assert_status(StatusCode::SEE_OTHER);
        let to = resp
            .header("location")
            .to_str()
            .expect("utf8 location")
            .to_string();
        let resp = self.get(&to).await;
        resp.assert_status_ok();
        resp
    }

    pub async fn get(&self, uri: &str) -> TestResponse {
        self.server.get(uri).await
    }

    pub async fn post(&self, uri: &str, body: &str) -> TestResponse {
        self.server
            .post(uri)
            .text(body)
            .content_type("application/x-www-form-urlencoded")
            .await
    }

    pub async fn post_htmx(&self, uri: &str, body: &str) -> TestResponse {
        self.server
            .post(uri)
            .text(body)
            .content_type("application/x-www-form-urlencoded")
            .add_header("HX-Request", "true")
            .await
    }
}

/// KC RB fixture with first/last/full names for FanDuel matching.
pub fn headshot_player(
    nfl_player_id: NflPlayerId,
    first: &str,
    last: &str,
    headshot_url: Option<&str>,
) -> Player {
    Player {
        gsis_id: nfl_player_id.0,
        espn_id: None,
        full_name: format!("{first} {last}"),
        first_name: Some(first.into()),
        last_name: Some(last.into()),
        position: Some("RB".into()),
        latest_team: Some(TeamAbbr::KC),
        headshot_url: headshot_url.map(Into::into),
    }
}

/// Weeks 1–2 2026 KC-vs-BUF games for headshot fixtures.
pub fn headshot_salary_games() -> Vec<Game> {
    [
        ("2026_01_BUF_KC", 1, datetime!(2026-09-13 17:00 UTC)),
        ("2026_02_BUF_KC", 2, datetime!(2026-09-20 17:00 UTC)),
    ]
    .into_iter()
    .map(|(id, week, kickoff)| Game {
        season_week: SeasonWeek {
            season: Season(2026),
            week: Week(week),
        },
        kickoff: Some(kickoff),
        ..factories::game_at(id, week, kickoff)
    })
    .collect()
}

/// Publish one week's salaries via the real admin upload.
/// `players` must be seeded; adds a KC defense row for validity.
pub async fn publish_headshot_slate(app: &TestApp, week: u8, players: &[Player]) {
    let mut csv = String::from(
        r#""Id","Position","First Name","Nickname","Last Name","FPPG","Played","Salary","Game","Team","Opponent","Injury Indicator","Injury Details"
"#,
    );
    for (index, player) in players.iter().enumerate() {
        csv.push_str(&format!(
            r#""{week}-{}","RB","{}","","{}","0","0","5000","BUF@KC","KC","BUF","",""
"#,
            u32::from(week) * 100 + index as u32 + 1,
            player.first_name.as_deref().unwrap_or_default(),
            player.last_name.as_deref().unwrap_or_default(),
        ));
    }
    csv.push_str(&format!(
        r#""{week}-dst","D","Kansas City","","Chiefs","0","0","3000","BUF@KC","KC","BUF","",""
"#
    ));

    let server = TestServer::new(Router::new().nest(
        "/nfl-data-admin",
        nfl_data::admin_router::<()>(
            Arc::clone(&app.nfl),
            AdminConfig {
                season: Season(2026),
                now_override: Some(datetime!(2026-09-01 00:00 UTC)),
            },
        ),
    ));
    let form = MultipartForm::new()
        .add_text("week", week.to_string())
        .add_part("csv", Part::bytes(csv.into_bytes()).file_name("salary.csv"));
    let response = server
        .post("/nfl-data-admin/salary-imports")
        .multipart(form)
        .await;
    response.assert_status(StatusCode::SEE_OTHER);
    assert_eq!(
        response.header("location").to_str().unwrap(),
        format!("/nfl-data-admin/salary-imports/2026{week:03}")
    );

    let slate = app
        .nfl
        .snapshot()
        .current_slate()
        .expect("the upload publishes a slate")
        .clone();
    assert_eq!(
        slate.season_week,
        SeasonWeek {
            season: Season(2026),
            week: Week(week),
        },
        "the published maximum slate is the uploaded week"
    );
}

/// Publish one week's salaries for `players` and `games`, through the real
/// admin upload, and return the week's main slate id.
///
/// The caller supplies the players and the week's games; this helper seeds
/// them, derives each row's DFS position, team, opponent, and matchup, and
/// uses unique deterministic FanDuel player IDs. Defense rows (salary 3000) are
/// generated for both teams of every supplied game.
pub async fn publish_slate(
    app: &TestApp,
    week: SeasonWeek,
    games: &[Game],
    players: &[Player],
) -> SlateId {
    app.nfl
        .seed_for_test(players, games)
        .await
        .expect("seed the slate's players and games");
    upload_slate(app, week, games, players).await
}

/// Re-upload after stat seeding: seeding alone does not publish a Snapshot.
pub async fn upload_slate(
    app: &TestApp,
    week: SeasonWeek,
    games: &[Game],
    players: &[Player],
) -> SlateId {
    let mut csv = String::from(
        r#""Id","Position","First Name","Nickname","Last Name","FPPG","Played","Salary","Game","Team","Opponent","Injury Indicator","Injury Details"
"#,
    );
    for (index, player) in players.iter().enumerate() {
        let team = player.latest_team.expect("fixture player has a team");
        let game = games
            .iter()
            .find(|game| {
                game.season_week == week && (game.home_team == team || game.away_team == team)
            })
            .expect("every fixture player's team plays this week");
        let opponent = if game.home_team == team {
            game.away_team
        } else {
            game.home_team
        };
        let position = match player.position.as_deref() {
            Some("QB") => "QB",
            Some("RB") => "RB",
            Some("WR") => "WR",
            Some("TE") => "TE",
            other => panic!("fixture player {index} has no QB/RB/WR/TE position: {other:?}"),
        };
        let (first, last) = match (&player.first_name, &player.last_name) {
            (Some(first), Some(last)) => (first.clone(), last.clone()),
            _ => {
                let mut parts = player.full_name.splitn(2, ' ');
                (
                    parts.next().unwrap_or_default().to_string(),
                    parts.next().unwrap_or_default().to_string(),
                )
            }
        };
        csv.push_str(&format!(
            r#""{}-{}","{position}","{first}","","{last}","0","0","5000","{}@{}","{}","{}","",""
"#,
            week.encode(),
            index + 1,
            game.away_team.as_str(),
            game.home_team.as_str(),
            team.as_str(),
            opponent.as_str(),
        ));
    }
    for (game_index, game) in games.iter().enumerate() {
        for (side, team) in [game.away_team, game.home_team].into_iter().enumerate() {
            let opponent = if team == game.home_team {
                game.away_team
            } else {
                game.home_team
            };
            csv.push_str(&format!(
                r#""{}-{game_index}-{side}-dst","D","{}","","Defense","0","0","3000","{}@{}","{}","{}","",""
"#,
                week.encode(),
                team.as_str(),
                game.away_team.as_str(),
                game.home_team.as_str(),
                team.as_str(),
                opponent.as_str(),
            ));
        }
    }

    let clock = games
        .iter()
        .filter_map(|game| game.kickoff)
        .min()
        .map(|kickoff| kickoff - Duration::hours(1))
        .unwrap_or_else(|| datetime!(2026-01-01 00:00 UTC));
    let server = TestServer::new(Router::new().nest(
        "/nfl-data-admin",
        nfl_data::admin_router::<()>(
            Arc::clone(&app.nfl),
            AdminConfig {
                season: week.season,
                now_override: Some(clock),
            },
        ),
    ));
    let form = MultipartForm::new()
        .add_text("week", week.week.0.to_string())
        .add_part("csv", Part::bytes(csv.into_bytes()).file_name("salary.csv"));
    let response = server
        .post("/nfl-data-admin/salary-imports")
        .multipart(form)
        .await;
    response.assert_status(StatusCode::SEE_OTHER);
    assert_eq!(
        response.header("location").to_str().unwrap(),
        format!("/nfl-data-admin/salary-imports/{}", week.encode())
    );

    let expected = SlateId(i64::from(week.encode()) * 10 + 1);
    assert!(
        app.nfl.snapshot().slate(&expected).is_some(),
        "the upload populates the week's main slate {expected:?}"
    );
    expected
}
