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
pub mod nfl_player_headshots;
pub mod nfl_teams;
mod observability;
pub(crate) mod player_identity;
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

/// Default filter; a set `RUST_LOG` replaces it.
const DEFAULT_LOG_FILTER: &str = "warn,sunday_slate=info,nfl_data=info,nflverse_data=info,tank01_data=info,fanduel_data=info,utils=info";

pub fn init_tracing() -> anyhow::Result<()> {
    use std::io::IsTerminal;
    use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

    let filter = match std::env::var("RUST_LOG") {
        Ok(value) => EnvFilter::try_new(&value)
            .map_err(|error| anyhow::anyhow!("invalid RUST_LOG {value:?}: {error}"))?,
        Err(std::env::VarError::NotPresent) => EnvFilter::new(DEFAULT_LOG_FILTER),
        Err(error) => return Err(anyhow::anyhow!("RUST_LOG is not valid Unicode: {error}")),
    };

    // journald and pipes are not terminals.
    let ansi = std::io::stdout().is_terminal();

    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer().with_ansi(ansi))
        .try_init()?;
    Ok(())
}
