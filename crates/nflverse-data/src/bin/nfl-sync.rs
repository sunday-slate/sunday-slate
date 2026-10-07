use std::process::ExitCode;

use clap::Parser;
use nflverse_data::{NflverseData, NflverseDataConfig};
use utils::Secret;

#[derive(Parser)]
#[command(about = "Sync nflverse data into the local SQLite cache")]
struct Args {
    #[arg(
        long,
        env = "NFLVERSE_DATABASE_URL",
        default_value = "sqlite://storage/nflverse-data.db"
    )]
    database_url: String,

    /// Earliest season to fetch (default: current UTC year - 2)
    #[arg(long, env = "NFLVERSE_EARLIEST_SEASON")]
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
                .unwrap_or_else(|_| "nflverse_data=info,nfl_sync=info".into()),
        )
        .init();

    let args = Args::parse();
    let mut config = NflverseDataConfig {
        database_url: args.database_url,
        github_token: args.github_token.map(Secret::new),
        ..Default::default()
    };
    if let Some(season) = args.earliest_season {
        config.earliest_season = season;
    }

    run(config, &mut std::io::stdout(), &mut std::io::stderr()).await
}

async fn run(
    config: NflverseDataConfig,
    stdout: &mut impl std::io::Write,
    stderr: &mut impl std::io::Write,
) -> ExitCode {
    let nfl = match NflverseData::connect(config).await {
        Ok(nfl) => nfl,
        Err(e) => {
            let _ = writeln!(stderr, "error: {e}");
            return ExitCode::FAILURE;
        }
    };
    match nfl.sync().await {
        Ok(report) => {
            if write!(stdout, "{report}").is_err() {
                return ExitCode::FAILURE;
            }
            if report.all_ok() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Err(e) => {
            let _ = writeln!(stderr, "error: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{header, method, path},
    };

    async fn mocked_run(fail_players: bool) -> (ExitCode, String, String) {
        let server = MockServer::start().await;
        for tag in [
            "schedules",
            "players",
            "weekly_rosters",
            "stats_player",
            "pbp",
        ] {
            let mut assets = Vec::new();
            let fixture = match tag {
                "schedules" => Some((
                    "games.csv",
                    include_bytes!("../../tests/fixtures/games.csv").as_slice(),
                )),
                "players" if !fail_players => Some((
                    "players.csv",
                    include_bytes!("../../tests/fixtures/players.csv").as_slice(),
                )),
                _ => None,
            };
            if let Some((name, bytes)) = fixture {
                Mock::given(method("GET"))
                    .and(path(format!("/{name}")))
                    .respond_with(ResponseTemplate::new(200).set_body_bytes(bytes))
                    .expect(1)
                    .mount(&server)
                    .await;
                assets.push(serde_json::json!({"name": name,
                    "updated_at": "2026-01-01T00:00:00Z",
                    "browser_download_url": format!("{}/{name}", server.uri())}));
            }
            let response = if tag == "players" && fail_players {
                ResponseTemplate::new(503)
            } else {
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"assets": assets}))
            };
            Mock::given(method("GET"))
                .and(path(format!(
                    "/repos/nflverse/nflverse-data/releases/tags/{tag}"
                )))
                .and(header("authorization", "Bearer sentinel"))
                .respond_with(response)
                .expect(1)
                .mount(&server)
                .await;
        }
        let dir = tempfile::tempdir().unwrap();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let exit = run(
            NflverseDataConfig {
                database_url: format!("sqlite://{}/cache.db", dir.path().display()),
                github_api_base: server.uri(),
                earliest_season: 2025,
                github_token: Some(Secret::new("sentinel")),
            },
            &mut stdout,
            &mut stderr,
        )
        .await;
        (
            exit,
            String::from_utf8(stdout).unwrap(),
            String::from_utf8(stderr).unwrap(),
        )
    }

    #[tokio::test]
    async fn success_prints_dataset_report_and_exits_successfully() {
        let (exit, stdout, stderr) = mocked_run(false).await;
        assert_eq!(exit, ExitCode::SUCCESS);
        assert!(stdout.contains("schedules"));
        assert!(stdout.contains("players"));
        assert!(!stdout.contains("FAILED"));
        assert!(!stdout.contains("sentinel"));
        assert!(stderr.is_empty());
    }

    #[tokio::test]
    async fn partial_failure_prints_report_and_exits_with_failure() {
        let (exit, stdout, stderr) = mocked_run(true).await;
        assert_eq!(exit, ExitCode::FAILURE);
        assert!(stdout.contains("schedules"));
        assert!(stdout.contains("FAILED"));
        assert!(!stdout.contains("sentinel"));
        assert!(stderr.is_empty());
    }

    #[tokio::test]
    async fn setup_error_prints_error_and_exits_with_failure() {
        let dir = tempfile::tempdir().unwrap();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let exit = run(
            NflverseDataConfig {
                database_url: format!("sqlite://{}/missing/cache.db", dir.path().display()),
                ..Default::default()
            },
            &mut stdout,
            &mut stderr,
        )
        .await;
        assert_eq!(exit, ExitCode::FAILURE);
        assert!(stdout.is_empty());
        assert!(String::from_utf8(stderr).unwrap().starts_with("error:"));
    }
}
