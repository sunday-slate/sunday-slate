use std::collections::HashMap;
use std::sync::Arc;

use axum::{
    extract::State,
    response::{IntoResponse, Response},
};
use axum_htmx::HxRequest;

use crate::{NflData, store::player_link, web::error::AdminError};

use super::view::{NflverseFillResult, NflverseTemplate};

pub(crate) async fn show(
    HxRequest(is_htmx): HxRequest,
    State(nfl): State<Arc<NflData>>,
) -> Result<Response, AdminError> {
    render(is_htmx, &nfl).await
}

pub(crate) async fn start(
    HxRequest(is_htmx): HxRequest,
    State(nfl): State<Arc<NflData>>,
) -> Result<Response, AdminError> {
    nfl.nflverse
        .request_refresh()
        .await
        .map_err(crate::NflDataError::Nflverse)?;
    render(is_htmx, &nfl).await
}

pub(crate) async fn fill_missing_espn_ids(
    HxRequest(is_htmx): HxRequest,
    State(nfl): State<Arc<NflData>>,
) -> Result<Response, AdminError> {
    let missing = player_link::gsis_ids_missing_espn(&nfl.store).await?;
    let players = nfl
        .nflverse
        .players_by_gsis(&missing)
        .await
        .map_err(crate::NflDataError::Nflverse)?;
    let updates: HashMap<_, String> = missing
        .iter()
        .filter_map(|id| {
            let espn_player_id = players
                .get(id.as_ref())?
                .espn_id
                .as_deref()
                .map(str::trim)
                .filter(|espn| !espn.is_empty())?;
            Some((id.clone(), espn_player_id.to_owned()))
        })
        .collect();
    let updated = tokio::spawn({
        let nfl = Arc::clone(&nfl);
        async move { nfl.apply_espn_id_fill(&updates).await }
    })
    .await??;

    let html = if is_htmx {
        askama::Template::render(&NflverseFillResult {
            fill_missing: player_link::count_missing_espn_ids(&nfl.store).await?,
            fill_updated: Some(updated),
        })?
    } else {
        let mut template = NflverseTemplate::load(&nfl).await?;
        template.fill_updated = Some(updated);
        askama::Template::render(&template)?
    };
    Ok(axum::response::Html(html).into_response())
}

async fn render(is_htmx: bool, nfl: &NflData) -> Result<Response, AdminError> {
    let html = if is_htmx {
        let template = NflverseTemplate::load_panel(nfl).await?;
        askama::Template::render(&template.as_panel())?
    } else {
        let template = NflverseTemplate::load(nfl).await?;
        askama::Template::render(&template)?
    };
    Ok(axum::response::Html(html).into_response())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::{Router, http::StatusCode};
    use axum_test::TestServer;
    use nfl_model::Player;

    use crate::{
        AdminConfig,
        store::player_link::{self, PlayerLink},
        test::test_support::mock_nfl,
    };

    fn player(gsis_id: &str, espn_id: &str, full_name: &str) -> Player {
        Player {
            gsis_id: gsis_id.into(),
            espn_id: Some(espn_id.into()),
            full_name: full_name.into(),
            first_name: None,
            last_name: None,
            position: None,
            latest_team: None,
            headshot_url: None,
        }
    }

    fn link(fd_player_id: u32, gsis_player_id: &str, espn_player_id: Option<&str>) -> PlayerLink {
        PlayerLink {
            fd_player_id,
            gsis_player_id: gsis_player_id.into(),
            espn_player_id: espn_player_id.map(str::to_owned),
        }
    }

    #[tokio::test]
    async fn fill_missing_espn_ids_action_persists_and_publishes() {
        let (nfl, upstream) = mock_nfl(None).await;
        nfl.seed_for_test(
            &[
                player("00-test", "1234", "Test Player"),
                player("00-saved", "888", "Saved Player"),
            ],
            &[],
        )
        .await
        .unwrap();
        player_link::save_links(
            &nfl.store,
            &[
                link(1, "00-test", None),
                link(2, "00-absent", None),
                link(3, "00-saved", Some("999")),
            ],
        )
        .await
        .unwrap();

        let admin = TestServer::new(Router::new().nest(
            "/nfl-data-admin",
            crate::web::router::<()>(
                Arc::clone(&nfl),
                AdminConfig {
                    season: nfl.season(),
                    now_override: None,
                },
            ),
        ));
        let before = nfl.snapshot();

        let page = admin.get("/nfl-data-admin/nflverse").await;
        assert_eq!(page.status_code(), StatusCode::OK);
        let body = page.text();
        assert!(body.contains("<span>Missing ESPN IDs: 2</span>"));
        assert!(!body.contains("Updated:"));

        let response = admin
            .post("/nfl-data-admin/nflverse/fill-missing-espn-ids")
            .add_header("HX-Request", "true")
            .await;
        assert_eq!(response.status_code(), StatusCode::OK);
        assert_eq!(
            response.text().trim(),
            "<span>Missing ESPN IDs: 1</span> <span>Updated: 1</span>"
        );
        assert_eq!(
            player_link::links_by_espn(nfl.store.reader())
                .await
                .unwrap(),
            std::collections::HashMap::from([
                ("1234".to_owned(), "00-test".to_owned()),
                ("999".to_owned(), "00-saved".to_owned()),
            ]),
            "the cached id fills, the uncached row stays blank, and the filled row is not selected again"
        );
        assert!(
            !Arc::ptr_eq(&before, &nfl.snapshot()),
            "updates rebuild and publish the snapshot"
        );

        let after = nfl.snapshot();
        let page = admin
            .post("/nfl-data-admin/nflverse/fill-missing-espn-ids")
            .await;
        assert_eq!(page.status_code(), StatusCode::OK);
        let body = page.text();
        assert!(
            body.contains("<!doctype html>"),
            "a plain POST renders the page"
        );
        assert!(body.contains(r#"id="nfl-espn-fill-result" role="status""#));
        assert!(body.contains("Missing ESPN IDs: 1"));
        assert!(body.contains("Updated: 0"));
        assert!(
            Arc::ptr_eq(&after, &nfl.snapshot()),
            "a zero-update fill publishes nothing"
        );
        assert!(
            upstream.received_requests().await.unwrap().is_empty(),
            "the cache is never refreshed"
        );
    }

    #[tokio::test]
    async fn fill_missing_espn_ids_retries_failed_publication() {
        let (nfl, upstream) = mock_nfl(None).await;
        nfl.seed_for_test(&[player("00-test", "1234", "Test Player")], &[])
            .await
            .unwrap();
        player_link::save_links(&nfl.store, &[link(1, "00-test", None)])
            .await
            .unwrap();

        let admin = TestServer::new(Router::new().nest(
            "/nfl-data-admin",
            crate::web::router::<()>(
                Arc::clone(&nfl),
                AdminConfig {
                    season: nfl.season(),
                    now_override: None,
                },
            ),
        ));
        let before = nfl.snapshot();

        nfl.store
            .write_tx(async |conn| {
                sqlx::query("ALTER TABLE slates RENAME TO slates_broken")
                    .execute(&mut *conn)
                    .await?;
                Ok::<(), sqlx::Error>(())
            })
            .await
            .unwrap();

        let response = admin
            .post("/nfl-data-admin/nflverse/fill-missing-espn-ids")
            .add_header("HX-Request", "true")
            .await;
        assert_eq!(response.status_code(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(response.text(), "Internal server error");
        assert_eq!(
            player_link::links_by_espn(nfl.store.reader())
                .await
                .unwrap(),
            std::collections::HashMap::from([("1234".to_owned(), "00-test".to_owned())]),
            "the write commits even though the publication failed"
        );
        assert!(
            Arc::ptr_eq(&before, &nfl.snapshot()),
            "a failed publication leaves readers on the old snapshot"
        );

        nfl.store
            .write_tx(async |conn| {
                sqlx::query("ALTER TABLE slates_broken RENAME TO slates")
                    .execute(&mut *conn)
                    .await?;
                Ok::<(), sqlx::Error>(())
            })
            .await
            .unwrap();

        let response = admin
            .post("/nfl-data-admin/nflverse/fill-missing-espn-ids")
            .add_header("HX-Request", "true")
            .await;
        assert_eq!(response.status_code(), StatusCode::OK);
        assert_eq!(
            response.text().trim(),
            "<span>Missing ESPN IDs: 0</span> <span>Updated: 0</span>"
        );
        assert!(
            !Arc::ptr_eq(&before, &nfl.snapshot()),
            "a zero-update retry still discharges the pending publication"
        );

        let after_retry = nfl.snapshot();
        let response = admin
            .post("/nfl-data-admin/nflverse/fill-missing-espn-ids")
            .add_header("HX-Request", "true")
            .await;
        assert_eq!(response.status_code(), StatusCode::OK);
        assert!(
            Arc::ptr_eq(&after_retry, &nfl.snapshot()),
            "once published, a zero-update fill publishes nothing"
        );
        assert!(
            upstream.received_requests().await.unwrap().is_empty(),
            "the cache is never refreshed"
        );
    }
}
