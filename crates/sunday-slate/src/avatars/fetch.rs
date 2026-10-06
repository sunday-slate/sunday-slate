use std::sync::Arc;
use std::time::Duration;

use tracing::warn;

use crate::media::{self, process::ProcessedImage, store as media_store};
use crate::nfl_players::store as player_store;
use crate::nfl_teams::store as team_store;
use crate::{AppError, AppState};
use utils::background::{Progress, Runner};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FetchReport {
    pub players_fetched: usize,
    pub teams_fetched: usize,
    pub failed: usize,
    pub skipped: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FetchProgress {
    pub total: usize,
    pub fetched: usize,
    pub failed: usize,
    pub skipped: usize,
}

pub type AvatarFetchRunner = Runner<FetchProgress, FetchReport>;

const MAX_HEADSHOT_BYTES: usize = 20 * 1024 * 1024;
const MAX_TEAM_LOGO_BYTES: usize = 5 * 1024 * 1024;
/// Initial body buffer; Content-Length is untrusted, so it only seeds up to this.
const INITIAL_BODY_CAPACITY: u64 = 1024 * 1024;

fn oversized_image_error(max_bytes: usize) -> String {
    format!("image response exceeds {} MB", max_bytes / (1024 * 1024))
}

pub fn espn_logo_url(abbr: &str) -> String {
    let slug = match abbr {
        "WAS" => "wsh".to_string(),
        other => other.to_lowercase(),
    };
    format!("https://a.espncdn.com/i/teamlogos/nfl/500/{slug}.png")
}

pub async fn fetch_missing(
    state: &AppState,
    progress: &Progress<FetchProgress, FetchReport>,
) -> Result<FetchReport, AppError> {
    fetch_missing_with(state, progress, espn_logo_url).await
}

/// Start a fetch in the background. Returns `false` if one is running.
pub fn start_fetch(state: &AppState) -> bool {
    let runner = Arc::clone(&state.avatar_runner);
    let state = state.clone();
    runner.try_start(FetchProgress::default(), move |progress| async move {
        fetch_missing(&state, &progress)
            .await
            .map_err(|error| error.to_string())
    })
}

async fn fetch_missing_with<F>(
    state: &AppState,
    progress: &Progress<FetchProgress, FetchReport>,
    team_url: F,
) -> Result<FetchReport, AppError>
where
    F: Fn(&str) -> String,
{
    let players = player_store::missing_headshots(state.db.reader()).await?;
    let teams = team_store::missing_logos(state.db.reader()).await?;
    let total = players.len() + teams.len();
    let mut report = FetchReport::default();
    publish(progress, total, &report);
    let player_ids: Vec<&str> = players
        .iter()
        .map(|player| player.gsis_player_id.as_str())
        .collect();
    let identities = if player_ids.is_empty() {
        std::collections::HashMap::new()
    } else {
        state.nfl.identify(&player_ids).await?
    };
    let client = reqwest::Client::new();

    for player in players {
        let Some(identity) = identities.get(&player.gsis_player_id) else {
            report.skipped += 1;
            publish(progress, total, &report);
            continue;
        };
        let Some(url) = identity.headshot_url.as_deref() else {
            report.skipped += 1;
            publish(progress, total, &report);
            continue;
        };
        let processed = match fetch_image(&client, url, MAX_HEADSHOT_BYTES).await {
            Ok(image) => image,
            Err(error) => {
                warn!(
                    gsis_player_id = %player.gsis_player_id,
                    url,
                    %error,
                    "failed to fetch player headshot"
                );
                report.failed += 1;
                publish(progress, total, &report);
                continue;
            }
        };
        let path = format!("headshots/{}.png", player.gsis_player_id);
        if let Err(error) = state
            .db
            .write_tx(async |conn| -> Result<(), AppError> {
                let media_id = save_avatar(conn, state, &path, &processed).await?;
                player_store::set_headshot_media_id(&mut *conn, player.id, media_id).await?;
                Ok(())
            })
            .await
        {
            warn!(
                gsis_player_id = %player.gsis_player_id,
                %error,
                "failed to save player headshot"
            );
            report.failed += 1;
            publish(progress, total, &report);
            continue;
        }
        report.players_fetched += 1;
        publish(progress, total, &report);
    }

    for team in teams {
        let url = team_url(&team.abbr);
        let processed = match fetch_image(&client, &url, MAX_TEAM_LOGO_BYTES).await {
            Ok(image) => image,
            Err(error) => {
                warn!(abbr = %team.abbr, url, %error, "failed to fetch team logo");
                report.failed += 1;
                publish(progress, total, &report);
                continue;
            }
        };
        let path = format!("logos/{}.png", team.abbr);
        if let Err(error) = state
            .db
            .write_tx(async |conn| -> Result<(), AppError> {
                let media_id = save_avatar(conn, state, &path, &processed).await?;
                team_store::set_logo_media_id(&mut *conn, &team.abbr, media_id).await?;
                Ok(())
            })
            .await
        {
            warn!(abbr = %team.abbr, %error, "failed to save team logo");
            report.failed += 1;
            publish(progress, total, &report);
            continue;
        }
        report.teams_fetched += 1;
        publish(progress, total, &report);
    }

    Ok(report)
}

/// Save an avatar at its stable path. A media row already at that path (for
/// example one left behind by an unlink) is reused, because `media.path` is
/// unique and a fresh insert would fail on every run.
async fn save_avatar(
    conn: &mut sqlx::SqliteConnection,
    state: &AppState,
    path: &str,
    processed: &ProcessedImage,
) -> Result<media::MediaId, AppError> {
    let existing = media_store::get_by_path(&mut *conn, path)
        .await?
        .map(|row| row.id);
    media::save_png(
        conn,
        &state.config.media_dir,
        existing,
        Some(path),
        &processed.png,
        processed.width,
        processed.height,
    )
    .await
}

/// Progress is the report so far plus the item count, so the panel and the
/// final report can never disagree.
fn publish(progress: &Progress<FetchProgress, FetchReport>, total: usize, report: &FetchReport) {
    progress.update(|p| {
        *p = FetchProgress {
            total,
            fetched: report.players_fetched + report.teams_fetched,
            failed: report.failed,
            skipped: report.skipped,
        }
    });
}

async fn read_bounded_image_body(
    response: reqwest::Response,
    max_bytes: usize,
) -> Result<Vec<u8>, String> {
    let content_length = response.content_length();
    if content_length.is_some_and(|length| length > max_bytes as u64) {
        return Err(oversized_image_error(max_bytes));
    }

    let mut response = response;
    let mut bytes = Vec::with_capacity(
        content_length
            .unwrap_or_default()
            .min(INITIAL_BODY_CAPACITY) as usize,
    );
    while let Some(chunk) = response.chunk().await.map_err(|error| error.to_string())? {
        if chunk.len() > max_bytes - bytes.len() {
            return Err(oversized_image_error(max_bytes));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

async fn fetch_image(
    client: &reqwest::Client,
    url: &str,
    max_bytes: usize,
) -> Result<ProcessedImage, String> {
    let response = client
        .get(url)
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .map_err(|error| error.to_string())?
        .error_for_status()
        .map_err(|error| error.to_string())?;
    let bytes = read_bounded_image_body(response, max_bytes).await?;
    tokio::task::spawn_blocking(move || media::process::process_avatar(&bytes))
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use image::{DynamicImage, ImageFormat, RgbImage};
    use sqlx::SqlitePool;
    use uuid::Uuid;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;
    use crate::media::{MediaId, store as media_store};
    use crate::nfl_players::store as player_store;
    use crate::tests::TestApp;
    use utils::background::{Progress, RunnerState};

    /// A progress handle bound to a runner that is `Running`, so `update`
    /// calls land somewhere a test can read back. The returned sender must
    /// be held for the lifetime of the test's `fetch_missing_with` call —
    /// dropping it early ends the job and returns the runner to `Idle`.
    fn test_progress(
        app: &TestApp,
    ) -> (
        Progress<FetchProgress, FetchReport>,
        tokio::sync::oneshot::Sender<()>,
    ) {
        let runner = app.state.avatar_runner.clone();
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        assert!(runner.try_start(FetchProgress::default(), |_| async move {
            let _ = rx.await;
            Ok(FetchReport::default())
        }));
        (Progress::for_test(runner), tx)
    }

    fn image_bytes(width: u32, height: u32, format: ImageFormat) -> Vec<u8> {
        let image = DynamicImage::ImageRgb8(RgbImage::new(width, height));
        let mut bytes = Cursor::new(Vec::new());
        image.write_to(&mut bytes, format).expect("encode image");
        bytes.into_inner()
    }

    async fn dummy_media(pool: &SqlitePool) -> MediaId {
        let path = format!("dummy-{}.png", Uuid::new_v4());
        media_store::insert(pool, &path, "image/png", 1, 1, 1)
            .await
            .expect("dummy media")
    }

    async fn link_teams(pool: &SqlitePool, missing: Option<&str>) {
        let media_id = dummy_media(pool).await;
        let mut query = sqlx::query("UPDATE nfl_teams SET logo_media_id = ?1");
        if let Some(abbr) = missing {
            query = sqlx::query("UPDATE nfl_teams SET logo_media_id = ?1 WHERE abbr <> ?2")
                .bind(media_id)
                .bind(abbr);
        } else {
            query = query.bind(media_id);
        }
        query.execute(pool).await.expect("link teams");
    }

    async fn add_player(
        app: &TestApp,
        gsis_id: &str,
        fd_id: &str,
        headshot_url: Option<&str>,
        seed_identity: bool,
    ) {
        player_store::upsert(&app.pool, gsis_id, fd_id)
            .await
            .expect("crosswalk");
        if seed_identity {
            app.nfl
                .seed_for_test(
                    &[nfl_data::Player {
                        gsis_id: gsis_id.into(),
                        espn_id: None,
                        full_name: "Test Player".into(),
                        first_name: None,
                        last_name: None,
                        position: Some("QB".into()),
                        latest_team: Some(nfl_data::TeamAbbr("KC".into())),
                        status: None,
                        birth_date: None,
                        headshot_url: headshot_url.map(str::to_owned),
                    }],
                    &[],
                )
                .await
                .expect("nfl identity");
        }
    }

    #[test]
    fn espn_logo_url_uses_nflverse_abbreviations() {
        assert_eq!(
            espn_logo_url("KC"),
            "https://a.espncdn.com/i/teamlogos/nfl/500/kc.png"
        );
        assert_eq!(
            espn_logo_url("WAS"),
            "https://a.espncdn.com/i/teamlogos/nfl/500/wsh.png"
        );
    }

    #[tokio::test]
    async fn fetch_missing_links_player_and_team_media() {
        let app = TestApp::new().await;
        let server = MockServer::start().await;
        let player_body = image_bytes(400, 200, ImageFormat::Png);
        let team_body = image_bytes(200, 400, ImageFormat::Png);
        Mock::given(method("GET"))
            .and(path("/player.png"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(player_body))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/team.png"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(team_body))
            .expect(1)
            .mount(&server)
            .await;
        add_player(
            &app,
            "00-FETCH",
            "fetch",
            Some(&format!("{}/player.png", server.uri())),
            true,
        )
        .await;
        link_teams(&app.pool, Some("KC")).await;

        let state = app.state();
        let team_url = format!("{}/team.png", server.uri());
        let (progress, _hold) = test_progress(&app);
        let report = fetch_missing_with(&state, &progress, |_| team_url.clone())
            .await
            .expect("fetch");

        assert_eq!(
            report,
            FetchReport {
                players_fetched: 1,
                teams_fetched: 1,
                failed: 0,
                skipped: 0,
            }
        );
        assert!(matches!(
            app.state.avatar_runner.snapshot(),
            RunnerState::Running {
                progress: FetchProgress {
                    total: 2,
                    fetched: 2,
                    failed: 0,
                    skipped: 0
                },
                ..
            }
        ));
        let player = player_store::by_fd_id(&app.pool, "fetch")
            .await
            .expect("player")
            .expect("crosswalk");
        let player_media = media_store::get(&app.pool, player.headshot_media_id.expect("headshot"))
            .await
            .expect("media")
            .expect("player media");
        assert_eq!((player_media.width, player_media.height), (256, 128));
        assert_eq!(player_media.path, "headshots/00-FETCH.png");
        assert!(state.config.media_dir.join(&player_media.path).is_file());
        let team_media_id: Option<i64> =
            sqlx::query_scalar("SELECT logo_media_id FROM nfl_teams WHERE abbr = 'KC'")
                .fetch_one(&app.pool)
                .await
                .expect("team media id");
        let team_media = media_store::get(&app.pool, MediaId(team_media_id.expect("logo")))
            .await
            .expect("media")
            .expect("team media");
        assert_eq!((team_media.width, team_media.height), (128, 256));
        assert_eq!(team_media.path, "logos/KC.png");
        assert!(state.config.media_dir.join(&team_media.path).is_file());
    }

    #[tokio::test]
    async fn fetch_missing_reuses_a_media_row_already_at_the_path() {
        let app = TestApp::new().await;
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/player.png"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(image_bytes(
                64,
                64,
                ImageFormat::Png,
            )))
            .expect(1)
            .mount(&server)
            .await;
        add_player(
            &app,
            "00-ORPHAN",
            "orphan",
            Some(&format!("{}/player.png", server.uri())),
            true,
        )
        .await;
        link_teams(&app.pool, None).await;
        let orphan =
            media_store::insert(&app.pool, "headshots/00-ORPHAN.png", "image/png", 1, 1, 1)
                .await
                .expect("orphan row");

        let state = app.state();
        let (progress, _hold) = test_progress(&app);
        let report = fetch_missing_with(&state, &progress, |_| String::new())
            .await
            .expect("fetch");

        assert_eq!(report.players_fetched, 1);
        assert_eq!(report.failed, 0);
        let player = player_store::by_fd_id(&app.pool, "orphan")
            .await
            .expect("player")
            .expect("crosswalk");
        assert_eq!(player.headshot_media_id, Some(orphan));
        let media = media_store::get(&app.pool, orphan)
            .await
            .expect("media")
            .expect("row");
        assert_eq!((media.width, media.height), (64, 64));
        assert!(state.config.media_dir.join(&media.path).is_file());
    }

    #[tokio::test]
    async fn fetch_missing_counts_http_and_process_failures() {
        let app = TestApp::new().await;
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/player.png"))
            .respond_with(ResponseTemplate::new(404))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/team.png"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"bad".to_vec()))
            .expect(1)
            .mount(&server)
            .await;
        add_player(
            &app,
            "00-FAIL",
            "fail",
            Some(&format!("{}/player.png", server.uri())),
            true,
        )
        .await;
        link_teams(&app.pool, Some("KC")).await;

        let team_url = format!("{}/team.png", server.uri());
        let (progress, _hold) = test_progress(&app);
        let report = fetch_missing_with(&app.state(), &progress, |_| team_url.clone())
            .await
            .expect("fetch");

        assert_eq!(report.failed, 2);
        assert_eq!(report.players_fetched, 0);
        assert_eq!(report.teams_fetched, 0);
        assert!(matches!(
            app.state.avatar_runner.snapshot(),
            RunnerState::Running {
                progress: FetchProgress {
                    total: 2,
                    fetched: 0,
                    failed: 2,
                    skipped: 0
                },
                ..
            }
        ));
        let player = player_store::by_fd_id(&app.pool, "fail")
            .await
            .expect("player")
            .expect("crosswalk");
        assert!(player.headshot_media_id.is_none());
        let team_media_id: Option<i64> =
            sqlx::query_scalar("SELECT logo_media_id FROM nfl_teams WHERE abbr = 'KC'")
                .fetch_one(&app.pool)
                .await
                .expect("team media id");
        assert!(team_media_id.is_none());
    }

    #[tokio::test]
    async fn fetch_missing_rejects_oversized_player_and_team_bodies() {
        let app = TestApp::new().await;
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/player.png"))
            .respond_with(
                ResponseTemplate::new(200).set_body_bytes(vec![0; MAX_HEADSHOT_BYTES + 1]),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/team.png"))
            .respond_with(
                ResponseTemplate::new(200).set_body_bytes(vec![0; MAX_TEAM_LOGO_BYTES + 1]),
            )
            .expect(1)
            .mount(&server)
            .await;
        add_player(
            &app,
            "00-OVERSIZED",
            "oversized",
            Some(&format!("{}/player.png", server.uri())),
            true,
        )
        .await;
        link_teams(&app.pool, Some("KC")).await;

        let state = app.state();
        let team_url = format!("{}/team.png", server.uri());
        let (progress, _hold) = test_progress(&app);
        let report = fetch_missing_with(&state, &progress, |_| team_url.clone())
            .await
            .expect("fetch");

        assert_eq!(
            report,
            FetchReport {
                players_fetched: 0,
                teams_fetched: 0,
                failed: 2,
                skipped: 0,
            }
        );
        let player = player_store::by_fd_id(&app.pool, "oversized")
            .await
            .expect("player")
            .expect("crosswalk");
        assert!(player.headshot_media_id.is_none());
        let team_media_id: Option<i64> =
            sqlx::query_scalar("SELECT logo_media_id FROM nfl_teams WHERE abbr = 'KC'")
                .fetch_one(&app.pool)
                .await
                .expect("team media id");
        assert!(team_media_id.is_none());
        let media_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM media")
            .fetch_one(&app.pool)
            .await
            .expect("media count");
        assert_eq!(media_count, 1);
        assert!(
            std::fs::read_dir(&state.config.media_dir)
                .expect("media directory")
                .next()
                .is_none()
        );
    }

    #[tokio::test]
    async fn fetch_missing_counts_absent_and_no_source_identities() {
        let app = TestApp::new().await;
        add_player(&app, "00-NO-SOURCE", "no-source", None, true).await;
        add_player(&app, "00-ABSENT", "absent", None, false).await;
        link_teams(&app.pool, None).await;

        let (progress, _hold) = test_progress(&app);
        let report = fetch_missing_with(&app.state(), &progress, |_| {
            "http://127.0.0.1:1/team.png".into()
        })
        .await
        .expect("fetch");

        assert_eq!(report.skipped, 2);
        assert_eq!(report.players_fetched, 0);
        assert_eq!(report.teams_fetched, 0);
        assert_eq!(report.failed, 0);
    }

    #[tokio::test]
    async fn fetch_missing_counts_save_failures() {
        let app = TestApp::new().await;
        let server = MockServer::start().await;
        let player_body = image_bytes(400, 200, ImageFormat::Png);
        Mock::given(method("GET"))
            .and(path("/player.png"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(player_body))
            .expect(1)
            .mount(&server)
            .await;
        add_player(
            &app,
            "00-SAVEFAIL",
            "savefail",
            Some(&format!("{}/player.png", server.uri())),
            true,
        )
        .await;
        link_teams(&app.pool, None).await;

        let state = app.state();
        std::fs::create_dir_all(&state.config.media_dir).expect("media dir");
        std::fs::write(state.config.media_dir.join("headshots"), b"not a directory")
            .expect("block headshots dir");

        let (progress, _hold) = test_progress(&app);
        let report =
            fetch_missing_with(&state, &progress, |_| "http://127.0.0.1:1/team.png".into())
                .await
                .expect("fetch");

        assert_eq!(
            report,
            FetchReport {
                players_fetched: 0,
                teams_fetched: 0,
                failed: 1,
                skipped: 0,
            }
        );
        let player = player_store::by_fd_id(&app.pool, "savefail")
            .await
            .expect("player")
            .expect("crosswalk");
        assert!(player.headshot_media_id.is_none());
    }

    #[tokio::test]
    async fn fetch_missing_skips_already_linked_entities() {
        let app = TestApp::new().await;
        let media_id = dummy_media(&app.pool).await;
        add_player(
            &app,
            "00-LINKED",
            "linked",
            Some("http://127.0.0.1:1/player.png"),
            true,
        )
        .await;
        player_store::set_headshot_media_id(&app.pool, 1, media_id)
            .await
            .expect("link player");
        link_teams(&app.pool, None).await;
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&server)
            .await;

        let (progress, _hold) = test_progress(&app);
        let report = fetch_missing_with(&app.state(), &progress, |_| {
            format!("{}/unexpected.png", server.uri())
        })
        .await
        .expect("fetch");

        assert_eq!(report, FetchReport::default());
    }

    #[tokio::test]
    async fn start_fetch_runs_in_the_background_and_records_the_report() {
        let app = TestApp::new().await;
        link_teams(&app.pool, None).await;
        add_player(&app, "00-NOSRC", "nosrc", None, true).await;

        let state = app.state();
        assert!(start_fetch(&state));
        let last = state.avatar_runner.wait_idle().await.expect("last run");
        assert_eq!(
            last.outcome,
            Ok(FetchReport {
                players_fetched: 0,
                teams_fetched: 0,
                failed: 0,
                skipped: 1
            })
        );
    }
}
