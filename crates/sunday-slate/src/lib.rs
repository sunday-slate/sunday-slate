#![forbid(unsafe_code)]
pub mod admin;
pub mod app;
pub mod assets;
pub mod auth;
pub mod avatars;
pub mod background;
pub mod chrome;
mod config;
pub mod contests;
pub mod context;
#[cfg(any(feature = "test-support", test))]
pub mod db;
#[cfg(not(any(feature = "test-support", test)))]
mod db;
pub mod draft_entry;
pub mod entries;
mod error;
pub mod fantasy_teams;
pub mod flash;
pub mod health;
pub mod home;
pub mod injuries;
pub mod invites;
pub mod leagues;
pub mod live;
pub mod mail;
pub mod media;
pub mod nfl_players;
pub mod nfl_teams;
pub(crate) mod player_identity;
pub mod player_salaries;
mod router;
pub mod rules;
pub mod scoring;
pub mod sessions;
pub mod standings;
mod state;
pub mod users;
pub mod validation;
pub mod web;

#[cfg(test)]
mod tests;

pub use app::App;
pub use config::Config;
pub use db::Db;
pub use error::AppError;
pub use state::AppState;
pub use users::User;

pub fn init_tracing() -> anyhow::Result<()> {
    use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

    tracing_subscriber::registry()
        .with(EnvFilter::new(std::env::var("RUST_LOG").unwrap_or_else(
            |_| "axum_login=debug,tower_sessions=debug,sqlx=warn,tower_http=debug".into(),
        )))
        .with(tracing_subscriber::fmt::layer())
        .try_init()?;
    Ok(())
}
