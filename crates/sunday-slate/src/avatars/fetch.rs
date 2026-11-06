use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tracing::warn;

use crate::media::{self, process::ProcessedImage, store as media_store};
use crate::nfl_player_headshots::service as headshot_service;
use crate::nfl_player_headshots::store as player_store;
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
    let players = headshot_service::missing_headshots(state).await?;
    let teams = team_store::missing_logos(state.db.reader()).await?;
    let total = players.len() + teams.len();
    let mut report = FetchReport::default();
    publish(progress, total, &report);
    let identities = if players.is_empty() {
        HashMap::new()
    } else {
        state.nfl.identify(&players).await?
    };
    let client = reqwest::Client::new();

    for player in players {
        let Some(identity) = identities.get(&player) else {
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
                    nfl_player_id = %player.0,
                    url,
                    %error,
                    "failed to fetch player headshot"
                );
                report.failed += 1;
                publish(progress, total, &report);
                continue;
            }
        };
        let path = format!("headshots/{}.png", player.0);
        if let Err(error) = state
            .db
            .write_tx(async |conn| -> Result<(), AppError> {
                let media_id = save_avatar(conn, state, &path, &processed).await?;
                player_store::set_headshot_media_id(&mut *conn, &player, media_id).await?;
                Ok(())
            })
            .await
        {
            warn!(
                nfl_player_id = %player.0,
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
    use nfl_model::NflPlayerId;
    use sqlx::SqlitePool;
    use uuid::Uuid;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;
    use crate::media::{MediaId, store as media_store};
    use crate::tests::TestApp;
    use crate::tests::{headshot_player, headshot_salary_games, publish_headshot_slate};
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

    async fn team_logo_id(pool: &SqlitePool) -> Option<i64> {
        sqlx::query_scalar("SELECT logo_media_id FROM nfl_teams WHERE abbr = 'KC'")
            .fetch_one(pool)
            .await
            .expect("team media id")
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
    async fn fetch_missing_links_team_media() {
        let app = TestApp::new().await;
        let server = MockServer::start().await;
        let team_body = image_bytes(200, 400, ImageFormat::Png);
        Mock::given(method("GET"))
            .and(path("/team.png"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(team_body))
            .expect(1)
            .mount(&server)
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
                teams_fetched: 1,
                failed: 0,
                skipped: 0,
            }
        );
        assert!(matches!(
            app.state.avatar_runner.snapshot(),
            RunnerState::Running {
                progress: FetchProgress {
                    total: 1,
                    fetched: 1,
                    failed: 0,
                    skipped: 0
                },
                ..
            }
        ));
        let team_media = media_store::get(
            &app.pool,
            MediaId(team_logo_id(&app.pool).await.expect("logo")),
        )
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
            .and(path("/team.png"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(image_bytes(
                64,
                64,
                ImageFormat::Png,
            )))
            .expect(1)
            .mount(&server)
            .await;
        link_teams(&app.pool, Some("KC")).await;
        let orphan = media_store::insert(&app.pool, "logos/KC.png", "image/png", 1, 1, 1)
            .await
            .expect("orphan row");

        let state = app.state();
        let team_url = format!("{}/team.png", server.uri());
        let (progress, _hold) = test_progress(&app);
        let report = fetch_missing_with(&state, &progress, |_| team_url.clone())
            .await
            .expect("fetch");

        assert_eq!(report.teams_fetched, 1);
        assert_eq!(report.failed, 0);
        assert_eq!(team_logo_id(&app.pool).await, Some(orphan.0));
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
            .and(path("/team.png"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"bad".to_vec()))
            .expect(1)
            .mount(&server)
            .await;
        link_teams(&app.pool, Some("KC")).await;

        let team_url = format!("{}/team.png", server.uri());
        let (progress, _hold) = test_progress(&app);
        let report = fetch_missing_with(&app.state(), &progress, |_| team_url.clone())
            .await
            .expect("fetch");

        assert_eq!(report.failed, 1);
        assert_eq!(report.players_fetched, 0);
        assert_eq!(report.teams_fetched, 0);
        assert!(matches!(
            app.state.avatar_runner.snapshot(),
            RunnerState::Running {
                progress: FetchProgress {
                    total: 1,
                    fetched: 0,
                    failed: 1,
                    skipped: 0
                },
                ..
            }
        ));
        assert!(team_logo_id(&app.pool).await.is_none());
    }

    #[tokio::test]
    async fn fetch_missing_rejects_oversized_team_body() {
        let app = TestApp::new().await;
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/team.png"))
            .respond_with(
                ResponseTemplate::new(200).set_body_bytes(vec![0; MAX_TEAM_LOGO_BYTES + 1]),
            )
            .expect(1)
            .mount(&server)
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
                failed: 1,
                skipped: 0,
            }
        );
        assert!(team_logo_id(&app.pool).await.is_none());
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
    async fn start_fetch_runs_in_the_background_and_records_the_report() {
        let app = TestApp::new().await;
        link_teams(&app.pool, None).await;

        let state = app.state();
        assert!(start_fetch(&state));
        let last = state.avatar_runner.wait_idle().await.expect("last run");
        assert_eq!(last.outcome, Ok(FetchReport::default()));
    }

    async fn headshot_media_id(pool: &SqlitePool, nfl_player_id: &NflPlayerId) -> Option<MediaId> {
        sqlx::query_scalar(
            "SELECT headshot_media_id FROM nfl_player_headshots WHERE nfl_player_id = ?1",
        )
        .bind(nfl_player_id)
        .fetch_optional(pool)
        .await
        .expect("headshot link")
    }

    async fn media_count(pool: &SqlitePool) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM media")
            .fetch_one(pool)
            .await
            .expect("media count")
    }

    async fn discovered_ids(app: &TestApp) -> Vec<NflPlayerId> {
        let mut ids = headshot_service::missing_headshots(&app.state())
            .await
            .expect("discovery");
        ids.sort_unstable();
        ids
    }

    /// Sorted typed ids for set assertions.
    fn nfl_player_ids(values: &[NflPlayerId]) -> Vec<NflPlayerId> {
        let mut ids = values.to_vec();
        ids.sort_unstable();
        ids
    }

    /// Week-1 old id, week-2 scenarios, and an unslated id.
    fn headshot_players(uri: &str) -> Vec<nfl_data::Player> {
        let url = |path: &str| format!("{uri}/{path}");
        vec![
            headshot_player(
                NflPlayerId("00-OLD".into()),
                "Old",
                "Timer",
                Some(&url("old.png")),
            ),
            headshot_player(
                NflPlayerId("00-CACHED".into()),
                "Cached",
                "Runner",
                Some(&url("cached.png")),
            ),
            headshot_player(
                NflPlayerId("00-NEW".into()),
                "New",
                "Runner",
                Some(&url("new.png")),
            ),
            headshot_player(
                NflPlayerId("00-FAIL".into()),
                "Fail",
                "Runner",
                Some(&url("fail.png")),
            ),
            headshot_player(NflPlayerId("00-NOSOURCE".into()), "No", "Source", None),
            headshot_player(
                NflPlayerId("00-UNSLATED".into()),
                "Un",
                "Slated",
                Some(&url("unslated.png")),
            ),
        ]
    }

    /// Seed players/games, publish weeks 1–2, prelink cached players and teams.
    async fn seed_headshot_scenario(app: &TestApp, uri: &str) -> Vec<nfl_data::Player> {
        let players = headshot_players(uri);
        app.nfl
            .seed_for_test(&players, &headshot_salary_games())
            .await
            .expect("seed provider");
        publish_headshot_slate(app, 1, &players[..1]).await;
        publish_headshot_slate(app, 2, &players[1..5]).await;

        let media = dummy_media(&app.pool).await;
        player_store::set_headshot_media_id(&app.pool, &NflPlayerId("00-CACHED".into()), media)
            .await
            .expect("prelink cached");
        player_store::set_headshot_media_id(
            &app.pool,
            &NflPlayerId("00-CACHEONLY".into()),
            dummy_media(&app.pool).await,
        )
        .await
        .expect("cache-only id");
        link_teams(&app.pool, None).await;
        players
    }

    fn shapless_team_url(_: &str) -> String {
        panic!("every team already has a logo")
    }

    #[tokio::test]
    async fn missing_headshots_follows_the_latest_published_slate() {
        let app = TestApp::new().await;
        // no snapshot yet
        assert!(discovered_ids(&app).await.is_empty());

        seed_headshot_scenario(&app, "http://assets.invalid").await;
        assert_eq!(
            discovered_ids(&app).await,
            nfl_player_ids(&[
                NflPlayerId("00-FAIL".into()),
                NflPlayerId("00-NEW".into()),
                NflPlayerId("00-NOSOURCE".into()),
            ]),
            "only uncached latest-slate players, without flex duplicates or the defense"
        );
    }

    #[tokio::test]
    async fn fetch_missing_downloads_only_uncached_latest_slate_players() {
        let app = TestApp::new().await;
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/new.png"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(image_bytes(
                200,
                400,
                ImageFormat::Png,
            )))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/fail.png"))
            .respond_with(ResponseTemplate::new(500))
            .expect(1)
            .mount(&server)
            .await;
        seed_headshot_scenario(&app, &server.uri()).await;
        let cached_id = headshot_media_id(&app.pool, &NflPlayerId("00-CACHED".into()))
            .await
            .expect("cached");
        // unlinked row at the stable path is reused
        let orphan = media_store::insert(&app.pool, "headshots/00-NEW.png", "image/png", 1, 1, 1)
            .await
            .expect("orphan row");

        let state = app.state();
        let (progress, _hold) = test_progress(&app);
        let report = fetch_missing_with(&state, &progress, shapless_team_url)
            .await
            .expect("fetch");

        assert_eq!(
            report,
            FetchReport {
                players_fetched: 1,
                teams_fetched: 0,
                failed: 1,
                skipped: 1,
            }
        );
        assert!(matches!(
            app.state.avatar_runner.snapshot(),
            RunnerState::Running {
                progress: FetchProgress {
                    total: 3,
                    fetched: 1,
                    failed: 1,
                    skipped: 1
                },
                ..
            }
        ));

        assert_eq!(
            headshot_media_id(&app.pool, &NflPlayerId("00-CACHED".into())).await,
            Some(cached_id),
            "a cached player keeps its link"
        );
        assert_eq!(
            headshot_media_id(&app.pool, &NflPlayerId("00-NEW".into())).await,
            Some(orphan)
        );
        let media = media_store::get(&app.pool, orphan)
            .await
            .expect("media")
            .expect("reused row");
        assert_eq!((media.width, media.height), (128, 256));
        assert_eq!(media.path, "headshots/00-NEW.png");
        let bytes = std::fs::read(state.config.media_dir.join(&media.path)).expect("saved png");
        let image = image::load_from_memory(&bytes).expect("a decodable png");
        assert_eq!((image.width(), image.height()), (128, 256));
        assert_eq!(
            headshot_media_id(&app.pool, &NflPlayerId("00-FAIL".into())).await,
            None
        );
        assert_eq!(
            headshot_media_id(&app.pool, &NflPlayerId("00-NOSOURCE".into())).await,
            None
        );

        let mut requested: Vec<String> = server
            .received_requests()
            .await
            .expect("requests")
            .into_iter()
            .map(|request| request.url.path().to_string())
            .collect();
        requested.sort_unstable();
        assert_eq!(
            requested,
            ["/fail.png", "/new.png"],
            "cached, old-slate, and unslated players are never downloaded"
        );

        assert_eq!(
            discovered_ids(&app).await,
            nfl_player_ids(&[
                NflPlayerId("00-FAIL".into()),
                NflPlayerId("00-NOSOURCE".into()),
            ]),
            "the downloaded player is cached; failed and sourceless ones retry"
        );
    }

    #[tokio::test]
    async fn fetch_missing_counts_malformed_player_images_as_failures() {
        let app = TestApp::new().await;
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/bad.png"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"bad".to_vec()))
            .expect(1)
            .mount(&server)
            .await;
        let player = headshot_player(
            NflPlayerId("00-BAD".into()),
            "Bad",
            "Image",
            Some(&format!("{}/bad.png", server.uri())),
        );
        app.nfl
            .seed_for_test(std::slice::from_ref(&player), &headshot_salary_games())
            .await
            .expect("seed provider");
        publish_headshot_slate(&app, 1, std::slice::from_ref(&player)).await;
        link_teams(&app.pool, None).await;

        let state = app.state();
        let (progress, _hold) = test_progress(&app);
        let report = fetch_missing_with(&state, &progress, shapless_team_url)
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
        assert_eq!(
            headshot_media_id(&app.pool, &NflPlayerId("00-BAD".into())).await,
            None
        );
        assert_eq!(media_count(&app.pool).await, 1, "only the team's dummy row");
    }

    #[tokio::test]
    async fn fetch_missing_rolls_back_when_the_media_file_cannot_be_written() {
        let app = TestApp::new().await;
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/save.png"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(image_bytes(
                64,
                64,
                ImageFormat::Png,
            )))
            .expect(1)
            .mount(&server)
            .await;
        let player = headshot_player(
            NflPlayerId("00-SAVE".into()),
            "Save",
            "Failure",
            Some(&format!("{}/save.png", server.uri())),
        );
        app.nfl
            .seed_for_test(std::slice::from_ref(&player), &headshot_salary_games())
            .await
            .expect("seed provider");
        publish_headshot_slate(&app, 1, std::slice::from_ref(&player)).await;
        link_teams(&app.pool, None).await;

        // a file at the media dir fails every write
        let media_dir = app.state.config.media_dir.clone();
        std::fs::remove_dir_all(&media_dir).expect("remove media dir");
        std::fs::write(&media_dir, b"not a directory").expect("replace media dir");

        let state = app.state();
        let (progress, _hold) = test_progress(&app);
        let report = fetch_missing_with(&state, &progress, shapless_team_url)
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
        assert_eq!(
            headshot_media_id(&app.pool, &NflPlayerId("00-SAVE".into())).await,
            None
        );
        assert_eq!(
            media_count(&app.pool).await,
            1,
            "the failed save committed no media row"
        );
    }
}
