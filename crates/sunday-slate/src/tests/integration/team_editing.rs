use crate::fantasy_teams::FantasyTeam;
use crate::fantasy_teams::store::{logo_media_id, team_for_user};
use crate::media::MediaId;
use crate::tests::factories::{self, LeagueOptions, TeamOptions, UserOptions};
use crate::tests::test_app::TestApp;
use crate::tests::utils::{attr_before, form_body, href_before, logo_form, png_bytes};
use axum_test::multipart::MultipartForm;
use http::StatusCode;
use sqlx::SqlitePool;

/// The stored `(name, owner_name)` of `team`.
async fn stored_names(pool: &SqlitePool, team: &FantasyTeam) -> (String, String) {
    let stored = team_for_user(pool, team.league_id, team.user_id)
        .await
        .expect("team row")
        .expect("team exists");
    (stored.name, stored.owner_name)
}

async fn stored_logo(pool: &SqlitePool, team_id: i64) -> Option<MediaId> {
    logo_media_id(pool, team_id).await.expect("logo row")
}

/// Journey: the owner follows the My Team tab to the team page, edits the
/// names, uploads a logo, then removes it.
#[tokio::test]
async fn owner_edits_names_and_logo_from_the_my_team_tab() {
    let app = TestApp::new().await;
    let gen_team = factories::team(
        &app.pool,
        TeamOptions {
            name: Some("Gridiron Gremlins".into()),
            owner_name: Some("Marcus".into()),
            ..Default::default()
        },
    )
    .await;
    app.login_as(&gen_team.owner).await;

    let home = app.get("/standings").await;
    home.assert_status_ok();
    let team_href = format!("/teams/{}", gen_team.team.id);
    // Standings rows also link to the team page, so scope to the tab bar.
    let home_body = home.text();
    let nav = home_body
        .split_once(r#"aria-label="Primary""#)
        .map(|(_, rest)| rest)
        .expect("tab bar renders");
    assert_eq!(
        href_before(nav, "My Team"),
        team_href,
        "the My Team tab should link to the team page: {nav}"
    );

    let detail = app.get(&team_href).await;
    detail.assert_status_ok();
    let detail_body = detail.text();
    let (header, main) = detail_body.split_once("<main").expect("app shell");
    let expected_edit_href = format!("/teams/{}/edit", gen_team.team.id);
    let navbar_edit = format!(
        r#"href="{}" class="btn btn-ghost -mr-2 text-primary">Edit</a>"#,
        expected_edit_href
    );
    assert!(
        header.contains(&navbar_edit),
        "navbar edit action: {header}"
    );
    assert!(
        !main.contains(&format!(r#"href="{}""#, expected_edit_href)),
        "team content edit link: {main}"
    );
    let edit_href = href_before(header, ">Edit</a>");
    assert_eq!(edit_href, expected_edit_href);
    let editor = app.get(&edit_href).await;
    editor.assert_status_ok();
    let edit_action = attr_before(&editor.text(), "action", "hx-post=");
    let saved = app
        .post(
            &edit_action,
            &form_body(&[
                ("name", "  Gridiron   Gremlins   Updated  "),
                ("owner_name", "  Marcus   Updated  "),
            ]),
        )
        .await;
    saved.assert_status(StatusCode::SEE_OTHER);
    assert_eq!(saved.header("location"), team_href);

    let updated = app.get(&team_href).await;
    updated.assert_status_ok();
    let updated_body = updated.text();
    let content = updated_body.as_str();
    assert!(content.contains("Gridiron Gremlins Updated"));
    assert!(content.contains("Marcus Updated"));
    let logo_href = href_before(&updated_body, r#"aria-label="Edit team logo""#);

    let logo_editor = app.get(&logo_href).await;
    logo_editor.assert_status_ok();
    let logo_action = attr_before(&logo_editor.text(), "action", "enctype=");
    let uploaded = app
        .server
        .post(&logo_action)
        .multipart(logo_form(png_bytes(300, 200)))
        .await;
    uploaded.assert_status(StatusCode::SEE_OTHER);
    assert_eq!(uploaded.header("location"), team_href);

    let with_logo = app.get(&team_href).await;
    with_logo.assert_status_ok();
    let with_logo_body = with_logo.text();
    let content = with_logo_body.as_str();
    let src = attr_before(content, "src", r#"alt="Team logo""#);
    assert!(src.starts_with("/media/"), "served media URL: {src}");
    app.get(&src).await.assert_status_ok();

    let board = app.get("/standings").await;
    board.assert_status_ok();
    let board_body = board.text();
    let content = board_body.as_str();
    assert!(content.contains(&src), "standings logo: {content}");

    let logo_editor = app.get(&logo_href).await;
    let editor_body = logo_editor.text();
    assert!(
        editor_body.contains(r#"name="remove" formnovalidate"#),
        "remove button: {editor_body}"
    );
    let removed = app
        .server
        .post(&attr_before(&editor_body, "action", "enctype="))
        .multipart(MultipartForm::new().add_text("remove", "true"))
        .await;
    removed.assert_status(StatusCode::SEE_OTHER);
    assert_eq!(removed.header("location"), team_href);

    let after = app.get(&team_href).await;
    after.assert_status_ok();
    let after_body = after.text();
    let content = after_body.as_str();
    assert!(!content.contains("/media/"), "logo removed: {content}");
    assert!(content.contains("GG"), "monogram restored: {content}");
}

#[tokio::test]
async fn name_editor_prefills_validates_and_updates() {
    let app = TestApp::new().await;
    let gen_team = factories::team(
        &app.pool,
        TeamOptions {
            name: Some("Gridiron Gremlins".into()),
            owner_name: Some("Marcus".into()),
            ..Default::default()
        },
    )
    .await;
    app.login_as(&gen_team.owner).await;
    let team = &gen_team.team;
    let team_id = team.id;
    let edit_path = format!("/teams/{team_id}/edit");

    let edit = app.get(&edit_path).await;
    edit.assert_status_ok();
    let edit_body = edit.text();
    assert_eq!(attr_before(&edit_body, "action", "hx-post="), edit_path);
    assert!(edit_body.contains(r#"value="Gridiron Gremlins""#));
    assert!(edit_body.contains(r#"value="Marcus""#));

    let invalid = form_body(&[("name", "   "), ("owner_name", "  Valid   Owner  ")]);
    let full = app.post(&edit_path, &invalid).await;
    full.assert_status_ok();
    let fragment = app.post_htmx(&edit_path, &invalid).await;
    fragment.assert_status_ok();
    let fragment_body = fragment.text();
    assert!(
        !fragment_body.contains("<html"),
        "fragment excludes page shell: {fragment_body}"
    );
    for body in [full.text(), fragment_body] {
        let owner_field = body.find("Your display name").expect("owner fieldset");
        let (name_field, owner_field) = body.split_at(owner_field);
        assert!(
            name_field.contains("length is lower than 1"),
            "name error: {name_field}"
        );
        assert!(
            owner_field.contains(r#"value="Valid Owner""#),
            "normalized: {owner_field}"
        );
        assert!(
            !owner_field.contains("text-error"),
            "owner has no error: {owner_field}"
        );
    }
    assert_eq!(
        stored_names(&app.pool, team).await,
        ("Gridiron Gremlins".to_owned(), "Marcus".to_owned())
    );

    let valid = app
        .post(
            &edit_path,
            &form_body(&[("name", "  New   Team  "), ("owner_name", "  New   Owner ")]),
        )
        .await;
    valid.assert_status(StatusCode::SEE_OTHER);
    assert_eq!(valid.header("location"), format!("/teams/{team_id}"));
    assert_eq!(
        stored_names(&app.pool, team).await,
        ("New Team".to_owned(), "New Owner".to_owned())
    );
}

/// Every team-editing route answers 404 for `team` and leaves it unchanged.
async fn assert_cannot_edit(app: &TestApp, pool: &SqlitePool, team: &FantasyTeam) {
    let team_id = team.id;
    for suffix in ["edit", "logo"] {
        app.get(&format!("/teams/{team_id}/{suffix}"))
            .await
            .assert_status(StatusCode::NOT_FOUND);
    }
    app.post(
        &format!("/teams/{team_id}/edit"),
        &form_body(&[("name", "Changed"), ("owner_name", "Changed")]),
    )
    .await
    .assert_status(StatusCode::NOT_FOUND);
    app.server
        .post(&format!("/teams/{team_id}/logo"))
        .multipart(logo_form(png_bytes(2, 2)))
        .await
        .assert_status(StatusCode::NOT_FOUND);

    assert_eq!(
        stored_names(pool, team).await,
        (team.name.clone(), team.owner_name.clone())
    );
    assert!(stored_logo(pool, team_id).await.is_none());
}

#[sqlx::test]
async fn mismatched_commissioner_cannot_edit_member_team(pool: SqlitePool) {
    let app = TestApp::from_pool(pool.clone()).await;
    let league = factories::league(&pool, LeagueOptions::default()).await;
    let member = factories::team(
        &pool,
        TeamOptions {
            league: Some(league.league.clone()),
            ..Default::default()
        },
    )
    .await;
    app.login_as(&league.commish).await;

    assert_cannot_edit(&app, &pool, &member.team).await;
}

/// The site admin can view any league, with no team of their own there.
/// The editing routes treat that like any other mismatch: 404, not 401.
#[sqlx::test]
async fn admin_without_a_team_in_the_league_gets_not_found(pool: SqlitePool) {
    let app = TestApp::from_pool(pool.clone()).await;
    let admin = factories::user(
        &pool,
        UserOptions {
            is_admin: true,
            ..Default::default()
        },
    )
    .await;
    factories::league(
        &pool,
        LeagueOptions {
            commish: Some(admin.user.clone()),
            ..Default::default()
        },
    )
    .await;
    let other = factories::team(&pool, TeamOptions::default()).await;
    app.login_as(&admin.user).await;

    assert_cannot_edit(&app, &pool, &other.team).await;
}

#[tokio::test]
async fn legacy_team_paths_have_no_compatibility_routes() {
    let app = TestApp::new().await;
    let gen_team = factories::team(&app.pool, TeamOptions::default()).await;
    app.login_as(&gen_team.owner).await;

    for path in ["/teams/me", "/teams/me/logo"] {
        let response = app.get(path).await;
        assert!(response.status_code().is_client_error(), "{path}");
        assert!(response.maybe_header("location").is_none(), "{path}");
    }
    for response in [
        app.server
            .post("/teams/me/logo")
            .multipart(logo_form(png_bytes(2, 2)))
            .await,
        app.post("/teams/me/logo/delete", "").await,
    ] {
        assert!(response.status_code().is_client_error());
        assert!(response.maybe_header("location").is_none());
    }
}

#[tokio::test]
async fn bad_uploads_leave_the_logo_alone_and_remove_wins() {
    let app = TestApp::new().await;
    let gen_team = factories::team(&app.pool, TeamOptions::default()).await;
    app.login_as(&gen_team.owner).await;
    let team_id = gen_team.team.id;
    let path = format!("/teams/{team_id}/logo");

    app.server
        .post(&path)
        .multipart(logo_form(png_bytes(10, 10)))
        .await
        .assert_status(StatusCode::SEE_OTHER);
    let linked = stored_logo(&app.pool, team_id).await;
    assert!(linked.is_some(), "valid logo linked");

    app.server
        .post(&path)
        .multipart(logo_form(b"not an image".to_vec()))
        .await
        .assert_status(StatusCode::BAD_REQUEST);
    assert_eq!(stored_logo(&app.pool, team_id).await, linked);

    let too_big = vec![0u8; 5 * 1024 * 1024 + 1024];
    app.server
        .post(&path)
        .multipart(logo_form(too_big))
        .await
        .assert_status(StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(stored_logo(&app.pool, team_id).await, linked);

    let remove_with_garbage = logo_form(b"not an image".to_vec()).add_text("remove", "true");
    app.server
        .post(&path)
        .multipart(remove_with_garbage)
        .await
        .assert_status(StatusCode::SEE_OTHER);
    assert!(stored_logo(&app.pool, team_id).await.is_none());
}
