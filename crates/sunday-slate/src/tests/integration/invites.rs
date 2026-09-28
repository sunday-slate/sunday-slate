use crate::tests::factories::{InviteOptions, LeagueOptions, UserOptions};
use crate::tests::utils::*;
use crate::tests::{TestApp, factories};
use fake::Fake;
use fake::faker::company::en::CompanyName;
use fake::faker::internet::en::{Password, SafeEmail};
use fake::faker::name::en::FirstName;
use http::StatusCode;
use url::Url;

/// Journey: new user receives an invite, accepts, creates an account, creates a
/// team, and appears on the standings. Verifies every step via link/URL
/// reachability from rendered output.
#[tokio::test]
async fn new_user_invite_journey_to_membership() {
    let mut app = TestApp::new().await;
    let gen_league = factories::league(&app.pool, LeagueOptions::default()).await;

    app.login_as(&gen_league.commish).await;

    // Commissioner sends an invite.
    let email: String = SafeEmail().fake();
    let resp = app.post("/invites", &form_body(&[("email", &email)])).await;
    resp.assert_status(StatusCode::SEE_OTHER);
    assert_eq!(resp.header("location"), "/invites");

    // Email captured; pull the token from the accept link.
    let sent = app.mailer.all();
    assert_eq!(sent.len(), 1);
    let token = token_from_link(&sent[0].html);

    // The invited user is not the commissioner: drop the commissioner's session
    // so the accept + create-user steps run as a fresh, anonymous visitor.
    app.clear_cookies();

    // The invite page renders details + an Accept link.
    let resp = app.get(&format!("/invite?token={token}")).await;
    resp.assert_status_ok();
    let body = resp.text();
    assert!(
        body.contains("/invite/accept?token="),
        "accept link: {body}"
    );

    // Accept (logged out, new email) routes to create-user.
    let resp = app.get(&format!("/invite/accept?token={token}")).await;
    resp.assert_status(StatusCode::SEE_OTHER);
    let loc = resp.header("location");
    let loc = loc.to_str().unwrap();
    assert!(loc.starts_with("/invite/create-user?token="), "loc: {loc}");

    // Set a password. This logs the new user in; the jar now carries their
    // session in place of the commissioner's.
    let password: String = Password(8..16).fake();
    let body = form_body(&[
        ("token", &token),
        ("password", &password),
        ("password_confirm", &password),
    ]);
    let resp = app.post("/invite/create-user", &body).await;
    resp.assert_status(StatusCode::SEE_OTHER);
    assert_eq!(resp.header("location"), "/teams/new");

    // The invite is still active (not accepted until team creation).
    let accepted: Option<String> =
        sqlx::query_scalar("SELECT accepted_at FROM invites WHERE email = ?")
            .bind(&email)
            .fetch_one(&app.pool)
            .await
            .expect("invite row");
    assert!(accepted.is_none(), "invite not yet accepted");

    // Create the team (as the newly-logged-in user).
    let team_name: String = CompanyName().fake();
    let owner_name: String = FirstName().fake();
    let resp = app
        .post(
            "/teams",
            &form_body(&[("name", &team_name), ("owner_name", &owner_name)]),
        )
        .await;
    resp.assert_status(StatusCode::SEE_OTHER);
    assert_eq!(resp.header("location"), "/");

    // Standings shows the new member; invite now accepted; new team is not commish.
    let resp = app.home().await;
    let body = resp.text();
    assert!(
        body.contains(&crate::tests::utils::as_rendered(&team_name)),
        "team listed: {body}"
    );

    let accepted: Option<String> =
        sqlx::query_scalar("SELECT accepted_at FROM invites WHERE email = ?")
            .bind(&email)
            .fetch_one(&app.pool)
            .await
            .expect("invite row");
    assert!(accepted.is_some(), "invite accepted after team creation");

    let is_commish: bool = sqlx::query_scalar(
        "SELECT is_commissioner FROM fantasy_teams ft JOIN users u ON u.id = ft.user_id \
         WHERE u.email = ?",
    )
    .bind(&email)
    .fetch_one(&app.pool)
    .await
    .expect("new team commish flag");
    assert!(!is_commish, "invitee is not the commissioner");
}

/// Journey: a failed regeneration still exposes a working replacement link
/// through the commissioner session and to an anonymous invitee.
#[tokio::test]
async fn invite_regeneration_email_failure_journey() {
    let mut app = TestApp::new_with_mailer(crate::tests::utils::failing_mailer()).await;
    let gen_league = factories::league(
        &app.pool,
        LeagueOptions {
            name: Some("Invite Failure Smoke".into()),
            ..Default::default()
        },
    )
    .await;
    let invite = factories::invite(
        &app.pool,
        InviteOptions {
            email: Some("invitee@example.com".into()),
            league: Some(gen_league.league.clone()),
        },
    )
    .await;

    app.login_as(&gen_league.commish).await;

    let home = app.home().await;
    let invites_href = href_before(&home.text(), "Invites");
    let invites = app.get(&invites_href).await;
    invites.assert_status_ok();
    let invites_body = invites.text();
    let row = list_item_containing(&invites_body, &invite.email);
    let action = attr_before(row, "action", "hx-post=");

    let response = app.post(&action, "").await;
    response.assert_status(StatusCode::SEE_OTHER);
    let location = response.header("location").to_str().unwrap().to_string();

    let listed = app.get(&location).await;
    listed.assert_status_ok();
    let listed_body = listed.text();
    let fresh_url = attr_before(&listed_body, "value", r#"x-ref="link""#);
    assert!(
        listed_body.contains("We couldn't email the new link."),
        "the commissioner must be told to send the link manually: {listed_body}"
    );

    let reloaded = app.get(&location).await;
    reloaded.assert_status_ok();
    let reloaded_body = reloaded.text();
    assert!(
        !reloaded_body.contains(&fresh_url),
        "the raw link is shown for one render only: {reloaded_body}"
    );

    app.clear_cookies();
    let parsed = Url::parse(&fresh_url).expect("fresh invite URL");
    let path_and_query = format!(
        "{}?{}",
        parsed.path(),
        parsed.query().expect("fresh invite URL query"),
    );
    let public = app.get(&path_and_query).await;
    public.assert_status_ok();
    let public_body = public.text();
    assert!(
        public_body.contains("Invite Failure Smoke"),
        "public invite identifies the seeded league: {public_body}"
    );
    assert!(
        public_body.contains("/invite/accept?token="),
        "public invite offers acceptance: {public_body}"
    );
}

/// Journey: an invitee to league B (while league A exists) accepts, creates
/// their team, and lands in league B.
#[tokio::test]
async fn invitee_to_second_league_lands_in_that_league() {
    let app = TestApp::new().await;
    let _league_a = factories::league(&app.pool, LeagueOptions::default()).await;
    let league_b = factories::league(
        &app.pool,
        LeagueOptions {
            name: Some("Backyard Bowl".into()),
            ..Default::default()
        },
    )
    .await;
    let gen_invite = factories::invite(
        &app.pool,
        InviteOptions {
            league: Some(league_b.league.clone()),
            email: Some("newbie@test.local".into()),
        },
    )
    .await;
    let token = gen_invite.token();

    // Accept (logged out, new email) routes to create-user.
    let resp = app.get(&format!("/invite/accept?token={token}")).await;
    resp.assert_status(StatusCode::SEE_OTHER);
    let loc = resp.header("location");
    let loc = loc.to_str().unwrap();
    assert!(loc.starts_with("/invite/create-user?token="), "loc: {loc}");

    // Set a password. This logs the new user in.
    let password: String = Password(8..16).fake();
    let body = form_body(&[
        ("token", &token),
        ("password", &password),
        ("password_confirm", &password),
    ]);
    let resp = app.post("/invite/create-user", &body).await;
    resp.assert_status(StatusCode::SEE_OTHER);
    assert_eq!(resp.header("location"), "/teams/new");

    app.get("/teams/new").await.assert_status_ok();
    let team = app
        .post("/teams", "name=Newbie+Squad&owner_name=Newbie")
        .await;
    team.assert_status(StatusCode::SEE_OTHER);

    let home = app.home().await;
    let html = home.text();
    assert!(html.contains("Backyard Bowl"));
    assert!(html.contains("Newbie Squad"));
}

/// Accepting a specific invite creates the team in that invite's league and
/// consumes that invite, even when the same email holds a newer active invite
/// in another league.
#[tokio::test]
async fn accepting_an_older_invite_lands_in_its_league() {
    let app = TestApp::new().await;
    let league_a = factories::league(&app.pool, LeagueOptions::default()).await;
    let league_b = factories::league(&app.pool, LeagueOptions::default()).await;
    let invite_a = factories::invite(
        &app.pool,
        InviteOptions {
            league: Some(league_a.league.clone()),
            email: Some("newbie@test.local".into()),
        },
    )
    .await;
    // Age the first invite so the league B invite is strictly newer.
    sqlx::query("UPDATE invites SET created_at = datetime('now','subsec','-1 hour') WHERE id = ?1")
        .bind(invite_a.id)
        .execute(&app.pool)
        .await
        .expect("age invite");
    factories::invite(
        &app.pool,
        InviteOptions {
            league: Some(league_b.league.clone()),
            email: Some("newbie@test.local".into()),
        },
    )
    .await;

    // Accept the OLDER invite (league A) and create the account.
    let token = invite_a.token();
    let resp = app.get(&format!("/invite/accept?token={token}")).await;
    resp.assert_status(StatusCode::SEE_OTHER);
    let password: String = Password(8..16).fake();
    let body = form_body(&[
        ("token", &token),
        ("password", &password),
        ("password_confirm", &password),
    ]);
    let resp = app.post("/invite/create-user", &body).await;
    resp.assert_status(StatusCode::SEE_OTHER);
    assert_eq!(resp.header("location"), "/teams/new");

    let team = app
        .post("/teams", "name=Newbie+Squad&owner_name=Newbie")
        .await;
    team.assert_status(StatusCode::SEE_OTHER);

    let team_league: i64 = sqlx::query_scalar(
        "SELECT ft.league_id FROM fantasy_teams ft
         JOIN users u ON u.id = ft.user_id WHERE u.email = 'newbie@test.local'",
    )
    .fetch_one(&app.pool)
    .await
    .expect("team league");
    assert_eq!(
        team_league, league_a.league.id,
        "the team belongs to the accepted invite's league"
    );
    let accepted: Vec<i64> =
        sqlx::query_scalar("SELECT id FROM invites WHERE accepted_at IS NOT NULL")
            .fetch_all(&app.pool)
            .await
            .expect("accepted invites");
    assert_eq!(
        accepted,
        vec![invite_a.id],
        "exactly the accepted invite is consumed"
    );
}

/// Journey: browse an invite while logged in as the wrong person, see the
/// mismatch screen with a log-out form, log out, and arrive back at the
/// invite page as anonymous with the accept link restored.
#[tokio::test]
async fn logout_from_mismatch_screen_returns_to_invite() {
    let app = TestApp::new().await;
    let gen_league = factories::league(&app.pool, LeagueOptions::default()).await;

    app.login_as(&gen_league.commish).await;

    let gen_user = factories::user(&app.pool, UserOptions::default()).await;

    let gen_invite = factories::invite(
        &app.pool,
        InviteOptions {
            league: Some(gen_league.league),
            email: Some(gen_user.user.email),
        },
    )
    .await;

    let token = gen_invite.token();
    let encoded_token = urlencoding::encode(&token);

    // Still logged in as commissioner: invite page shows mismatch.
    let resp = app.get(&format!("/invite?token={encoded_token}")).await;
    resp.assert_status_ok();
    let body = resp.text();
    assert!(
        body.contains(r#"action="/logout""#),
        "mismatch screen must offer a log-out form: {body}"
    );

    // Log out via the mismatch screen's form, redirecting back to the invite.
    let next_url = format!("/invite?token={encoded_token}");
    let logout_body = form_body(&[("next", &next_url)]);
    let resp = app.post("/logout", &logout_body).await;
    resp.assert_status(StatusCode::SEE_OTHER);
    let loc = resp.header("location").to_str().unwrap().to_string();
    assert_eq!(loc, next_url);

    // Now anonymous (the jar dropped the cleared session cookie): the invite page
    // renders the normal valid state again.
    let resp = app.get(&loc).await;
    resp.assert_status_ok();
    let body = resp.text();
    assert!(
        body.contains("/invite/accept?token="),
        "accept link restored once anonymous: {body}"
    );
}

/// The drawer's Invites entry is gated on the commissioner flag that
/// `context::request_ctx` derives. Pin it from both sides so a change to how
/// that flag is computed cannot silently hand every member commissioner
/// navigation, or strip it from the commissioner.
#[tokio::test]
async fn drawer_shows_invites_link_only_for_the_commissioner() {
    let app = TestApp::new().await;
    let gen_league = factories::league(&app.pool, LeagueOptions::default()).await;

    app.login_as(&gen_league.commish).await;
    let commish_view = app.get("/standings").await;
    commish_view.assert_status_ok();
    assert!(
        commish_view.text().contains(r#"href="/invites""#),
        "commissioner sees the Invites entry"
    );

    let member = factories::team(
        &app.pool,
        factories::TeamOptions {
            league: Some(gen_league.league.clone()),
            is_commish: false,
            ..Default::default()
        },
    )
    .await;
    app.login_as(&member.owner).await;
    let member_view = app.get("/standings").await;
    member_view.assert_status_ok();
    assert!(
        !member_view.text().contains(r#"href="/invites""#),
        "a non-commissioner member does not see the Invites entry"
    );
}

fn token_from_link(html: &str) -> String {
    let marker = "/invite?token=";
    let start = html.find(marker).expect("accept link present") + marker.len();
    let rest = &html[start..];
    let end = rest
        .find(['\"', '\'', '<', ' ', '\n'])
        .unwrap_or(rest.len());
    rest[..end].to_string()
}
