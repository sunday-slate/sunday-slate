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
    provider: Option<Tank01Client>,
}

impl App {
    pub async fn build(config: Config) -> anyhow::Result<Self> {
        let mailer = Mailer::from_config(&config)?;
        Self::build_with_mailer(config, mailer).await
    }

    pub async fn build_with_mailer(config: Config, mailer: Mailer) -> anyhow::Result<Self> {
        let provider = config
            .tank01_api_key
            .clone()
            .map(|key| Tank01Client::new(Tank01Config::new(key)))
            .transpose()?;
        let db = Db::open(&config.database_url).await?;
        std::fs::create_dir_all(&config.media_dir)?;
        let (session_layer, sessions) = sessions::layer(&config).await?;

        let nfl = Arc::new(
            nfl_data::NflData::connect(nfl_data::NflDataConfig {
                database_url: config.nfl_database_url.clone(),
                github_token: config.nfl_github_token.clone(),
                ..Default::default()
            })
            .await?,
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
            provider,
        })
    }

    pub async fn serve(self, bind_addr: &str) -> anyhow::Result<()> {
        let App {
            router,
            state,
            provider,
        } = self;
        let listener = tokio::net::TcpListener::bind(bind_addr).await?;
        tracing::info!("listening on http://{bind_addr}");

        let coordinator_state = state.clone();
        let deletion_task = tokio::task::spawn(
            state
                .sessions
                .continuously_delete_expired(Duration::from_secs(3600)),
        );
        let live_task = match provider {
            Some(provider) => Some(tokio::spawn(crate::live::run_coordinator(
                coordinator_state.clone(),
                provider,
            ))),
            None if coordinator_state.config.live_dev_feed => Some(tokio::spawn(
                crate::live::dev_feed::run(coordinator_state.clone()),
            )),
            None => None,
        };
        let coordinator_task = Arc::new(tokio::sync::Mutex::new(live_task));
        if let Some(interval) = state.config.nfl_sync_interval() {
            state.nfl.start_scheduler(interval);
        }
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
            state.nfl.stop_scheduler();
        }

        match deletion_task.await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::error!(error = ?e, "session sweeper exited with error"),
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
    nfl.stop_scheduler();
    if let Some(task) = coordinator_task.lock().await.take() {
        match task.await {
            Ok(()) => {}
            Err(error) if error.is_cancelled() => {
                tracing::info!("live coordinator aborted on shutdown")
            }
            Err(error) => tracing::error!(error = ?error, "live coordinator exited with error"),
        }
    }
}
