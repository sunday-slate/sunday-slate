use crate::auth::Commissioner;
use crate::invites::handlers::index::rows_for;
use crate::invites::model::InviteRow;
use crate::invites::service;
use crate::leagues::CurrentLeague;
use crate::web::redirect;
use crate::{AppError, AppState};
use askama::Template;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse};
use axum_htmx::HxRequest;
use tower_sessions::Session;

#[derive(Template)]
#[template(path = "invites/_row.html")]
struct InviteRowTemplate {
    row: InviteRow,
}

/// Mint a fresh accept link for an outstanding invite, attempt to email it once,
/// and show it so the commissioner can also send it by hand.
///
/// Email delivery is best effort after the replacement is committed, so a
/// failure still leaves the fresh link in the rendered row or session redirect.
/// 404 rather than 403 for another league's invite: the commissioner has no
/// business knowing whether that id exists.
pub async fn link(
    _: Commissioner,
    CurrentLeague(league): CurrentLeague,
    Path(id): Path<i64>,
    HxRequest(is_htmx): HxRequest,
    session: Session,
    State(state): State<AppState>,
) -> Result<impl IntoResponse, AppError> {
    let Some(link) = service::invite_link(
        &state.db,
        &state.mailer,
        &state.config.base_url,
        &league,
        id,
    )
    .await?
    else {
        return Ok(StatusCode::NOT_FOUND.into_response());
    };

    // htmx swaps the single row in place. Without it, hand the link to the
    // list through the session and redirect, so a reload cannot re-post.
    if !is_htmx {
        crate::invites::set_fresh_link(&session, id, link).await?;
        return Ok(redirect(false, "/invites"));
    }

    let rows = rows_for(&state.db, league.id, Some((id, link))).await?;
    // Only reachable if the invite was revoked between the two statements.
    let Some(row) = rows.into_iter().find(|row| row.invite.id == id) else {
        return Ok(StatusCode::NOT_FOUND.into_response());
    };
    Ok(Html(InviteRowTemplate { row }.render()?).into_response())
}

#[cfg(test)]
mod tests {
    use crate::invites::store;
    use crate::tests::factories::{InviteOptions, LeagueOptions, TeamOptions};
    use crate::tests::utils::invite_token_in;
    use crate::tests::{TestApp, factories};
    use http::StatusCode;

    /// POST /invites/{id}/link answers with a fresh link, emails it to the
    /// invitee, and shows it so the commissioner can paste it elsewhere.
    #[tokio::test]
    async fn link_hands_back_a_working_accept_link() {
        let app = TestApp::new().await;
        let gen_league = factories::league(&app.pool, LeagueOptions::default()).await;

        app.login_as(&gen_league.commish).await;

        let invite = factories::invite(
            &app.pool,
            InviteOptions {
                email: Some("invitee@example.com".to_string()),
                league: Some(gen_league.league),
            },
        )
        .await;

        let resp = app
            .post_htmx(&format!("/invites/{}/link", invite.id), "")
            .await;

        resp.assert_status_ok();
        let body = resp.text();
        let token = invite_token_in(&body);
        let sent = app.mailer.all();
        assert_eq!(sent.len(), 1, "regeneration emails the fresh link");
        assert_eq!(sent[0].to, invite.email);
        assert_eq!(invite_token_in(&sent[0].html), token);
        assert!(sent[0].text.contains(&token));
        assert!(
            body.contains("A new link was emailed."),
            "the row should confirm the email: {body}"
        );
        let verified = store::verify(&app.state.db, &token)
            .await
            .expect("verify")
            .expect("the offered link must accept the invite");
        assert_eq!(verified.invite_id, invite.id);
        assert!(
            body.contains("Only this link works now"),
            "the row must say the earlier link is retired: {body}"
        );
    }
    /// A mail failure still returns the committed replacement link so the
    /// commissioner can send it manually.
    #[tokio::test]
    async fn link_email_failure_still_shows_working_link() {
        let app = TestApp::new_with_mailer(crate::tests::utils::failing_mailer()).await;
        let gen_league = factories::league(&app.pool, LeagueOptions::default()).await;

        app.login_as(&gen_league.commish).await;

        let invite = factories::invite(
            &app.pool,
            InviteOptions {
                email: Some("invitee@example.com".to_string()),
                league: Some(gen_league.league),
            },
        )
        .await;
        let old_token = invite.token();

        let resp = app
            .post_htmx(&format!("/invites/{}/link", invite.id), "")
            .await;

        resp.assert_status_ok();
        let body = resp.text();
        let token = invite_token_in(&body);
        assert!(body.contains("We couldn't email the new link."));
        let verified = store::verify(&app.state.db, &token)
            .await
            .expect("verify")
            .expect("the offered link must accept the invite");
        assert_eq!(verified.invite_id, invite.id);
        assert!(
            store::verify(&app.state.db, &old_token)
                .await
                .expect("verify")
                .is_none(),
            "the old link must stop working after regeneration"
        );
    }

    /// The link a commissioner copies replaces whatever link was emailed
    /// earlier — the secret is hashed, so a fresh link means a fresh secret.
    #[tokio::test]
    async fn link_retires_the_previously_issued_token() {
        let app = TestApp::new().await;
        let gen_league = factories::league(&app.pool, LeagueOptions::default()).await;

        app.login_as(&gen_league.commish).await;

        let invite = factories::invite(
            &app.pool,
            InviteOptions {
                league: Some(gen_league.league),
                ..Default::default()
            },
        )
        .await;
        let emailed = invite.token();

        app.post_htmx(&format!("/invites/{}/link", invite.id), "")
            .await
            .assert_status_ok();

        assert!(
            store::verify(&app.state.db, &emailed)
                .await
                .expect("verify")
                .is_none(),
            "the emailed link must stop working once a new one is copied"
        );
    }

    /// Without htmx the link rides the session to the list, so a reload of
    /// the resulting page cannot re-post and mint yet another link.
    #[tokio::test]
    async fn link_without_htmx_redirects_and_shows_the_link_once() {
        let app = TestApp::new().await;
        let gen_league = factories::league(&app.pool, LeagueOptions::default()).await;

        app.login_as(&gen_league.commish).await;

        let invite = factories::invite(
            &app.pool,
            InviteOptions {
                league: Some(gen_league.league),
                ..Default::default()
            },
        )
        .await;

        let resp = app.post(&format!("/invites/{}/link", invite.id), "").await;
        resp.assert_status(StatusCode::SEE_OTHER);
        assert_eq!(resp.header("location"), "/invites");

        let listed = app.get("/invites").await.text();
        let token = invite_token_in(&listed);
        assert!(
            store::verify(&app.state.db, &token)
                .await
                .expect("verify")
                .is_some(),
            "the list must show a working link: {listed}"
        );
        assert!(
            listed.contains("Only this link works now"),
            "a copied link retires the earlier one: {listed}"
        );

        let again = app.get("/invites").await.text();
        assert!(
            !again.contains("/invite?token="),
            "the link must be shown only once: {again}"
        );
    }

    /// A league member who isn't the commissioner can't mint invite links.
    #[tokio::test]
    async fn link_is_commissioner_only() {
        let app = TestApp::new().await;
        let gen_team = factories::team(&app.pool, TeamOptions::default()).await;

        app.login_as(&gen_team.owner).await;

        let invite = factories::invite(&app.pool, InviteOptions::default()).await;

        let resp = app
            .post_htmx(&format!("/invites/{}/link", invite.id), "")
            .await;
        resp.assert_status(StatusCode::FORBIDDEN);
    }

    /// A commissioner can't mint a link for another league's invite.
    #[tokio::test]
    async fn link_declines_an_invite_from_another_league() {
        let app = TestApp::new().await;
        let mine = factories::league(&app.pool, LeagueOptions::default()).await;
        let theirs = factories::league(&app.pool, LeagueOptions::default()).await;

        app.login_as(&mine.commish).await;

        let invite = factories::invite(
            &app.pool,
            InviteOptions {
                league: Some(theirs.league),
                ..Default::default()
            },
        )
        .await;

        let emailed = invite.token();

        let resp = app
            .post_htmx(&format!("/invites/{}/link", invite.id), "")
            .await;
        resp.assert_status(StatusCode::NOT_FOUND);

        assert!(
            store::verify(&app.state.db, &emailed)
                .await
                .expect("verify")
                .is_some(),
            "a refused request must leave the other league's invite untouched"
        );
    }
}
