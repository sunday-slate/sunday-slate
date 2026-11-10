use anyhow::Context;
use axum::Router;
use std::sync::Arc;
use std::time::Duration;
use tank01_data::{Tank01Client, Tank01Config};
use tower_sessions::ExpiredDeletion;

use crate::mail::Mailer;
use crate::sessions;
use crate::{AppState, Config, Db};
pub struct App {
    pub router: Router,
    state: AppState,
    tank01: Option<Tank01Client>,
}

impl App {
    pub async fn build(config: Config) -> anyhow::Result<Self> {
        let mailer = Mailer::from_config(&config)?;
        Self::build_with_mailer(config, mailer).await
    }

    pub async fn build_with_mailer(config: Config, mailer: Mailer) -> anyhow::Result<Self> {
        let tank01 = config
            .tank01_api_key
            .clone()
            .map(|key| Tank01Client::new(Tank01Config::new(key)))
            .transpose()
            .context("creating the Tank01 client")?;
        let db = Db::open(&config.database_url)
            .await
            .context("opening the application database")?;
        std::fs::create_dir_all(&config.media_dir).with_context(|| {
            format!(
                "creating the media directory {}",
                config.media_dir.display()
            )
        })?;
        let (session_layer, sessions) = sessions::layer(&config)
            .await
            .context("opening the session store")?;

        let nfl = Arc::new(
            nfl_data::NflData::connect(nfl_data::NflDataConfig {
                nflverse_database_url: config.nflverse_database_url.clone(),
                fanduel_database_url: config.fanduel_database_url.clone(),
                database_url: config.nfl_data_database_url.clone(),
                season: nfl_model::Season(config.season),
                github_token: config.nflverse_github_token.clone(),
                refresh_interval: config.nflverse_sync_interval(),
                ..Default::default()
            })
            .await
            .context("connecting the NFL data store")?,
        );
        let avatar_runner = Arc::new(crate::avatars::AvatarFetchRunner::new());
        let live_enabled = config.tank01_api_key.is_some() || config.live_dev_feed;
        let live = Arc::new(crate::live::LiveContestHub::new(live_enabled));

        let state = AppState {
            db,
            sessions,
            config: Arc::new(config),
            mailer,
            nfl,
            avatar_runner,
            live,
            injuries: Arc::new(crate::injuries::InjuryReports::new()),
        };

        Ok(Self {
            state: state.clone(),
            router: crate::router::build(state, session_layer),
            tank01,
        })
    }

    pub async fn serve(self, bind_addr: &str) -> anyhow::Result<()> {
        let App {
            router,
            state,
            tank01,
        } = self;
        let listener = tokio::net::TcpListener::bind(bind_addr).await?;
        tracing::info!("listening on http://{bind_addr}");

        let coordinator_state = state.clone();
        let deletion_task = tokio::task::spawn({
            let state = state.clone();
            async move {
                let _exit = TaskExitGuard {
                    task: "session_sweeper",
                };
                let live = state.live.clone();
                match state
                    .sessions
                    .continuously_delete_expired(Duration::from_secs(3600))
                    .await
                {
                    Ok(()) if shutdown_signaled(&live) => {
                        tracing::info!("session sweeper stopped during shutdown")
                    }
                    Ok(()) => tracing::warn!("session sweeper returned unexpectedly"),
                    Err(error) => {
                        tracing::error!(error = ?error, "session sweeper exited with error")
                    }
                }
            }
        });
        let live_task = match tank01 {
            Some(tank01) => Some(spawn_supervised(
                "live_coordinator",
                state.live.clone(),
                crate::live::run_coordinator(coordinator_state.clone(), tank01),
            )),
            None if coordinator_state.config.live_dev_feed => Some(spawn_supervised(
                "live_dev_feed",
                state.live.clone(),
                crate::live::dev_feed::run(coordinator_state.clone()),
            )),
            None => None,
        };
        let coordinator_task = Arc::new(tokio::sync::Mutex::new(live_task));
        state.nfl.start_background_tasks();
        let serve_result = axum::serve(listener, router)
            .with_graceful_shutdown(wait_for_shutdown_signal(
                deletion_task.abort_handle(),
                state.live.clone(),
                Arc::clone(&coordinator_task),
                state.nfl.clone(),
            ))
            .await;

        if serve_result.is_err() {
            state.live.shutdown();
            deletion_task.abort();
            state.nfl.stop_background_tasks();
        }

        match deletion_task.await {
            // Outcomes were logged by the supervisor when the task ended.
            Ok(()) => {}
            Err(e) if e.is_cancelled() => tracing::info!("session sweeper aborted on shutdown"),
            Err(e) => return Err(e.into()),
        }
        if let Some(task) = coordinator_task.lock().await.take() {
            match task.await {
                Ok(()) => {}
                Err(e) if e.is_cancelled() => {
                    tracing::info!("live coordinator aborted on shutdown")
                }
                Err(e) => return Err(e.into()),
            }
        }

        serve_result?;
        tracing::info!("shutdown complete");
        Ok(())
    }
}

/// Whether the live hub has broadcast shutdown.
fn shutdown_signaled(live: &crate::live::LiveContestHub) -> bool {
    *live.shutdown_receiver().borrow()
}

/// Logs a panic escaping a supervised task while it unwinds.
struct TaskExitGuard {
    task: &'static str,
}

impl Drop for TaskExitGuard {
    fn drop(&mut self) {
        if std::thread::panicking() {
            tracing::error!(task = self.task, "background task panicked");
        }
    }
}

/// Spawn a long-lived task, logging panics and unexpected returns.
fn spawn_supervised<F>(
    task: &'static str,
    live: Arc<crate::live::LiveContestHub>,
    future: F,
) -> tokio::task::JoinHandle<()>
where
    F: Future<Output = ()> + Send + 'static,
{
    tokio::task::spawn(async move {
        let _exit = TaskExitGuard { task };
        future.await;
        if shutdown_signaled(&live) {
            tracing::info!(task, "background task stopped during shutdown");
        } else {
            tracing::warn!(task, "background task returned unexpectedly");
        }
    })
}

async fn wait_for_shutdown_signal(
    abort_handle: tokio::task::AbortHandle,
    live: Arc<crate::live::LiveContestHub>,
    coordinator_task: Arc<tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>>,
    nfl: Arc<nfl_data::NflData>,
) {
    let ctrl_c = async {
        match tokio::signal::ctrl_c().await {
            Ok(()) => {}
            Err(e) => {
                tracing::warn!(error = ?e, "failed to install Ctrl+C handler; use SIGTERM instead");
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }

    tracing::info!("shutdown signal received, draining");
    live.shutdown();
    abort_handle.abort();
    nfl.stop_background_tasks();
    if let Some(task) = coordinator_task.lock().await.take() {
        match task.await {
            Ok(()) => {}
            Err(error) if error.is_cancelled() => {
                tracing::info!("live coordinator aborted on shutdown")
            }
            // A non-cancelled JoinError is a panic, already logged while unwinding.
            Err(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::Router;
    use axum_test::TestServer;

    use super::App;
    use crate::mail::Mailer;
    use crate::tests::TestApp;

    #[tokio::test]
    async fn host_configuration_preserves_refresh_cadence() {
        let fixture = TestApp::new().await;
        let dir = tempfile::tempdir().unwrap();
        let mut config = (*fixture.state.config).clone();
        config.database_url = format!("sqlite://{}/app.db", dir.path().display());
        config.nflverse_database_url = format!("sqlite://{}/nflverse.db", dir.path().display());
        config.media_dir = dir.path().join("media");
        config.nflverse_sync_interval_secs = 3600;

        let app = App::build_with_mailer(
            config,
            Mailer::capture("Sunday Slate <no-reply@example.com>"),
        )
        .await
        .unwrap();
        let router = Router::new().nest(
            "/nfl-data-admin",
            nfl_data::admin_router::<()>(
                Arc::clone(&app.state.nfl),
                nfl_data::AdminConfig {
                    season: nfl_data::Season(app.state.config.season),
                    now_override: app.state.config.now_override,
                },
            ),
        );
        let server = TestServer::builder().build(router);
        let response = server.get("/nfl-data-admin/nflverse").await;
        response.assert_status_ok();
        assert!(response.text().contains("Automatic refresh every hour."));
    }
}
