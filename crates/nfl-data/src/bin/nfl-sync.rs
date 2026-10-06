use std::process::ExitCode;

use clap::Parser;
use nfl_data::{NflData, NflDataConfig};
use utils::Secret;

#[derive(Parser)]
#[command(about = "Sync nflverse data into the local SQLite cache")]
struct Args {
    #[arg(
        long,
        env = "NFL_DATABASE_URL",
        default_value = "sqlite://storage/nflverse-data.db"
    )]
    database_url: String,

    /// Earliest season to fetch (default: current UTC year - 2)
    #[arg(long, env = "NFL_EARLIEST_SEASON")]
    earliest_season: Option<u16>,

    /// GitHub token; raises the API rate limit, never required
    #[arg(long, env = "GITHUB_TOKEN", hide_env_values = true)]
    github_token: Option<String>,
}

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "nfl_data=info,nfl_sync=info".into()),
        )
        .init();

    let args = Args::parse();
    let mut config = NflDataConfig {
        database_url: args.database_url,
        github_token: args.github_token.map(Secret::new),
        ..Default::default()
    };
    if let Some(season) = args.earliest_season {
        config.earliest_season = season;
    }

    let nfl = match NflData::connect(config).await {
        Ok(nfl) => nfl,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    match nfl.sync().await {
        Ok(report) => {
            print!("{report}");
            if report.all_ok() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
