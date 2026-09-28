use crate::auth::Commissioner;
use crate::invites::store;
use crate::leagues::CurrentLeague;
use crate::web::redirect;
use crate::{AppError, AppState};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum_htmx::HxRequest;
use axum_messages::Messages;

pub async fn revoke(
    _: Commissioner,
    CurrentLeague(league): CurrentLeague,
    Path(id): Path<i64>,
    HxRequest(is_htmx): HxRequest,
    messages: Messages,
    State(state): State<AppState>,
) -> Result<impl IntoResponse, AppError> {
    state
        .db
        .write_tx(async |conn| -> Result<(), sqlx::Error> {
            store::revoke(&mut *conn, id, league.id).await
        })
        .await?;

    messages.success("Invite revoked.");

    if is_htmx {
        Ok(StatusCode::OK.into_response())
    } else {
        Ok(redirect(is_htmx, "/invites"))
    }
}

#[cfg(test)]
mod tests {
    use crate::tests::factories::{InviteOptions, LeagueOptions};

    use crate::tests::{TestApp, factories};

    use http::StatusCode;

    /// POST /invites/{id}/revoke (non-htmx) deletes the invite and redirects
    /// to the invite list.
    #[tokio::test]
    async fn revoke_removes_pending_invite_and_redirects() {
        let app = TestApp::new().await;
        let gen_league = factories::league(&app.pool, LeagueOptions::default()).await;

        app.login_as(&gen_league.commish).await;

        let id = factories::invite(
            &app.pool,
            InviteOptions {
                league: Some(gen_league.league),
                ..Default::default()
            },
        )
        .await
        .id;

        let resp = app.post(&format!("/invites/{id}/revoke"), "").await;

        resp.assert_status(StatusCode::SEE_OTHER);
        assert_eq!(resp.header("location"), "/invites");

        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM invites")
            .fetch_one(&app.pool)
            .await
            .expect("count");
        assert_eq!(count, 0);
    }

    /// POST /invites/{id}/revoke (htmx) returns 200 with no redirect, and the
    /// response body contains an OOB flash toast.
    #[tokio::test]
    async fn revoke_htmx_shows_flash_toast() {
        let app = TestApp::new().await;
        let gen_league = factories::league(&app.pool, LeagueOptions::default()).await;

        app.login_as(&gen_league.commish).await;

        let id = factories::invite(
            &app.pool,
            InviteOptions {
                league: Some(gen_league.league),
                ..Default::default()
            },
        )
        .await
        .id;

        let resp = app.post_htmx(&format!("/invites/{id}/revoke"), "").await;

        resp.assert_status_ok();
        assert!(
            resp.maybe_header("HX-Redirect").is_none(),
            "htmx revoke must stay on the page, not redirect"
        );
        let body = resp.text();
        assert!(
            body.contains("Invite revoked."),
            "expected flash toast in body: {body}"
        );
        assert!(
            body.contains(r#"hx-swap-oob="beforeend""#),
            "expected OOB swap wrapper: {body}"
        );

        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM invites")
            .fetch_one(&app.pool)
            .await
            .expect("count");
        assert_eq!(count, 0, "htmx revoke should remove the invite row");
    }
}
