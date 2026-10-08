use std::sync::Arc;

use axum::http::StatusCode;
use axum_test::{TestResponse, TestServer};
use sqlx::SqlitePool;

use crate::mail::Mailer;
use crate::tests::factories::{self, GeneratedUser, UserOptions};
use crate::{AppState, Config, Db, User};

pub struct TestApp {
    pub server: TestServer,
    pub router: axum::Router,
    pub pool: SqlitePool,
    pub mailer: Mailer,
    pub nfl: Arc<nfl_data::NflData>,
    pub state: AppState,
    // Keeps the per-test media directory alive; removed on drop.
    pub _media_dir: tempfile::TempDir,
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

    /// Like `from_pool` but with the config clock pinned to `now`.
    pub async fn from_pool_at(pool: SqlitePool, now: time::OffsetDateTime) -> Self {
        Self::build(
            pool,
            Mailer::capture("Sunday Slate <no-reply@example.com>"),
            Some(now),
            false,
            None,
        )
        .await
    }

    /// Like `from_pool_at` but with the dev live feed flag set.
    pub async fn from_pool_at_with_live_dev_feed(
        pool: SqlitePool,
        now: time::OffsetDateTime,
        live_dev_feed: bool,
    ) -> Self {
        Self::build(
            pool,
            Mailer::capture("Sunday Slate <no-reply@example.com>"),
            Some(now),
            live_dev_feed,
            None,
        )
        .await
    }

    async fn build(
        pool: SqlitePool,
        mailer: Mailer,
        now_override: Option<time::OffsetDateTime>,
        live_dev_feed: bool,
        nfl_override: Option<Arc<nfl_data::NflData>>,
    ) -> Self {
        let media_dir = tempfile::tempdir().expect("temp media dir");

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
            season: 2025,
            media_dir: media_dir.path().to_path_buf(),
            smtp: None,
            now_override,
        };

        let (session_layer, sessions) = crate::sessions::layer_in_memory()
            .await
            .expect("build session layer");

        let db = Db::test(pool.clone());

        let nfl = match nfl_override {
            Some(nfl) => nfl,
            None => Arc::new(
                nfl_data::NflData::in_memory()
                    .await
                    .expect("in-memory nfl-data"),
            ),
        };

        let avatar_runner = Arc::new(crate::avatars::AvatarFetchRunner::new());

        let state = AppState {
            db,
            sessions,
            config: Arc::new(config),
            mailer: mailer.clone(),
            nfl: nfl.clone(),
            avatar_runner,
            live: Arc::new(crate::live::LiveContestHub::new(true)),
            injuries: Arc::new(crate::injuries::InjuryReports::new()),
        };

        let router = crate::router::build(state.clone(), session_layer);

        // Cookie jar on: each response's Set-Cookie is saved and replayed on the
        // next request from this server, so login/flash/logout carry automatically.
        let server = TestServer::builder().save_cookies().build(router.clone());

        TestApp {
            server,
            router,
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
        Self::from_pool(crate::tests::utils::in_memory_pool().await).await
    }

    /// Build a host test app around a mock-configured NFL-data instance.
    pub async fn new_with_nfl(nfl: Arc<nfl_data::NflData>) -> Self {
        Self::build(
            crate::tests::utils::in_memory_pool().await,
            Mailer::capture("Sunday Slate <no-reply@example.com>"),
            None,
            false,
            Some(nfl),
        )
        .await
    }

    /// Like `new` but accepts a custom `Mailer` (e.g. `failing_mailer()`).
    pub async fn new_with_mailer(mailer: Mailer) -> Self {
        Self::from_pool_with_mailer(crate::tests::utils::in_memory_pool().await, mailer).await
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

    /// GET carrying one extra request header — htmx requests, mainly.
    pub async fn get_with_header(&self, uri: &str, name: &str, value: &str) -> TestResponse {
        self.server.get(uri).add_header(name, value).await
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
