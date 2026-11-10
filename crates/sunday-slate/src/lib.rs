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

use nfl_data::{SeasonType, SeasonWeek};

pub use app::App;
pub use config::Config;
pub use db::Db;
pub use error::AppError;
pub use state::AppState;
pub use users::User;

/// The classification a season week implies: weeks 1–18 are regular season and
/// week 19 onward is postseason. Provider rows still carry the enum explicitly,
/// so derive it from the week instead of storing it twice.
pub(crate) fn season_type_of(week: SeasonWeek) -> SeasonType {
    if week.postseason() {
        SeasonType::Post
    } else {
        SeasonType::Reg
    }
}

/// Default filter; a non-blank `RUST_LOG` replaces it.
const DEFAULT_LOG_FILTER: &str = "warn,sunday_slate=info,nfl_data=info,nflverse_data=info,tank01_data=info,fanduel_data=info,utils=info";

/// Build the subscriber filter. Unset and blank values fall back to the
/// default: a blank value parses to a filter that admits nothing.
fn log_filter(value: Option<&str>) -> anyhow::Result<tracing_subscriber::EnvFilter> {
    use tracing_subscriber::EnvFilter;

    match value {
        None => Ok(EnvFilter::new(DEFAULT_LOG_FILTER)),
        Some(value) if value.trim().is_empty() => Ok(EnvFilter::new(DEFAULT_LOG_FILTER)),
        Some(value) => EnvFilter::try_new(value)
            .map_err(|error| anyhow::anyhow!("invalid RUST_LOG {value:?}: {error}")),
    }
}

pub fn init_tracing() -> anyhow::Result<()> {
    use std::io::IsTerminal;
    use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

    let filter = match std::env::var("RUST_LOG") {
        Ok(value) => log_filter(Some(&value))?,
        Err(std::env::VarError::NotPresent) => log_filter(None)?,
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

#[cfg(test)]
mod logging_tests {
    use super::{DEFAULT_LOG_FILTER, log_filter};

    fn default_rendered() -> String {
        tracing_subscriber::EnvFilter::new(DEFAULT_LOG_FILTER).to_string()
    }

    #[test]
    fn unset_and_blank_rust_log_use_the_default_filter() {
        let default = default_rendered();
        for value in [None, Some(""), Some("   \n\t ")] {
            let filter = log_filter(value).expect("default filter parses");
            assert_eq!(filter.to_string(), default, "{value:?}");
        }
    }

    #[test]
    fn a_set_rust_log_replaces_the_default_filter() {
        let filter = log_filter(Some("error")).expect("valid filter");
        assert_eq!(filter.to_string(), "error");
        assert_ne!(filter.to_string(), default_rendered());
    }

    #[test]
    fn an_invalid_rust_log_is_reported() {
        let error = log_filter(Some("sunday_slate=loud")).expect_err("invalid level");
        let message = error.to_string();
        assert!(message.contains("RUST_LOG"), "{message}");
        assert!(message.contains("loud"), "{message}");
    }
}
