use crate::tests::factories::{LeagueOptions, TeamOptions, UserOptions};
use crate::tests::utils::{as_rendered, href_before};
use crate::tests::{TestApp, factories};

const ACTIVE: &str = r#"aria-current="page""#;

/// Signed-out chrome: non-link brand text and a plain brand title suffix.
#[tokio::test]
async fn login_page_shows_unlinked_brand() {
    let app = TestApp::new().await;
    factories::user(&app.pool, factories::UserOptions::default()).await;

    let resp = app.get("/login").await;
    resp.assert_status_ok();

    let body = resp.text();
    assert!(
        body.contains("<title>Sign in – Sunday Slate</title>"),
        "title should fall back to brand when signed out: {body}"
    );
    let navbar = body
        .split_once("navbar-center")
        .map(|(_, rest)| rest)
        .expect("page should render the navbar");
    let center = &navbar[..navbar
        .find("navbar-end")
        .expect("navbar-end follows center")];
    assert!(
        center.contains("Sunday Slate") && !center.contains("<a "),
        "bar center should be non-link brand text: {center}"
    );
}

/// The tab bar marks exactly the active tab: standings, the fixed
/// Contests tab, and the viewer's own team page (My Team). League-scoped
/// tabs carry the league name as a bar subtitle; the team page titles its
/// bar with the league name outright, so its `<title>` skips the suffix.
#[tokio::test]
async fn tab_bar_marks_the_active_tab() {
    let app = TestApp::new().await;
    let gen_league = factories::league(&app.pool, LeagueOptions::default()).await;
    app.login_as(&gen_league.commish).await;
    let team_path = format!("/teams/{}", gen_league.commish_team.id);
    let team_name = as_rendered(&gen_league.commish_team.name);
    let league = as_rendered(&gen_league.league.name);

    for (path, active_href, title) in [
        ("/standings", "/standings", "Standings"),
        ("/contests", "/contests", "Contests"),
    ] {
        let resp = app.get(path).await;
        resp.assert_status_ok();
        let body = resp.text();
        assert_eq!(
            body.matches(ACTIVE).count(),
            1,
            "{path}: exactly one tab should be active: {body}"
        );
        assert_eq!(
            href_before(&body, ACTIVE),
            active_href,
            "{path}: active tab should link to {active_href}: {body}"
        );
        assert!(
            body.contains(&format!("<title>{title} – {league}</title>")),
            "{path}: title should carry the league suffix: {body}"
        );
        assert!(
            body.contains(&format!(r#"text-base-content/50">{league}</div>"#)),
            "{path}: the bar should carry the league name as a subtitle: {body}"
        );
    }

    let resp = app.get(&team_path).await;
    resp.assert_status_ok();
    let body = resp.text();
    assert_eq!(
        body.matches(ACTIVE).count(),
        1,
        "{team_path}: exactly one tab should be active: {body}"
    );
    assert_eq!(
        href_before(&body, ACTIVE),
        team_path,
        "{team_path}: My Team should light: {body}"
    );
    assert!(
        body.contains(&format!("<title>{league}</title>")),
        "{team_path}: the bar titles the league, with no doubled suffix: {body}"
    );
    assert!(
        body.contains(&team_name),
        "{team_path}: the band still carries the team name: {body}"
    );
}

/// When the page title already names the league (here, a team sharing the
/// league's name) the `<title>` suffix must not repeat it.
#[tokio::test]
async fn title_suffix_does_not_double_when_the_page_title_names_the_league() {
    let app = TestApp::new().await;
    let gen_league = factories::league(
        &app.pool,
        LeagueOptions {
            name: Some("Ironwood League".into()),
            ..Default::default()
        },
    )
    .await;
    let gen_team = factories::team(
        &app.pool,
        TeamOptions {
            league: Some(gen_league.league.clone()),
            name: Some("Ironwood League".into()),
            ..Default::default()
        },
    )
    .await;
    app.login_as(&gen_league.commish).await;

    let resp = app.get(&format!("/teams/{}", gen_team.team.id)).await;
    resp.assert_status_ok();
    let body = resp.text();
    assert!(
        body.contains("<title>Ironwood League</title>"),
        "title should not double the league name: {body}"
    );
    assert!(
        !body.contains("<title>Ironwood League – Ironwood League</title>"),
        "title should not double the league name: {body}"
    );
}

/// `/` dispatches to the Current Contest, where the Current tab lights and
/// links back to `/`.
#[tokio::test]
async fn current_tab_lights_when_home_dispatches_to_a_contest() {
    let app = TestApp::from_pool_at(
        crate::tests::utils::in_memory_pool().await,
        time::macros::datetime!(2025 - 09 - 06 12:00 UTC),
    )
    .await;
    let gen_league = factories::league(&app.pool, LeagueOptions::default()).await;
    factories::contest_with_games(&app.pool, "Week 1", &["2025_01_BUF_KC"]).await;
    let g = factories::game_at(
        "2025_01_BUF_KC",
        1,
        time::macros::datetime!(2025 - 09 - 07 17:00 UTC),
    );
    app.nfl.seed_for_test(&[], &[g]).await.unwrap();
    app.login_as(&gen_league.commish).await;

    let resp = app.home().await;
    resp.assert_status_ok();
    let body = resp.text();
    assert_eq!(
        body.matches(ACTIVE).count(),
        1,
        "exactly one tab should be active: {body}"
    );
    assert_eq!(
        href_before(&body, ACTIVE),
        "/",
        "the Current tab should link to /: {body}"
    );
}

/// A contest with no games attached is never the Current Contest, so its
/// detail page lights Contests, not Current.
#[tokio::test]
async fn contest_detail_lights_contests_tab_when_not_current() {
    let app = TestApp::new().await;
    let gen_team = factories::team(&app.pool, TeamOptions::default()).await;
    app.login_as(&gen_team.owner).await;
    let wk1 = factories::contest(&app.pool, "Week 1").await;

    let resp = app.get(&format!("/contests/{wk1}")).await;
    resp.assert_status_ok();
    let body = resp.text();
    assert_eq!(
        body.matches(ACTIVE).count(),
        1,
        "exactly one tab should be active: {body}"
    );
    assert_eq!(
        href_before(&body, ACTIVE),
        "/contests",
        "a contest with no games is never Current: {body}"
    );
}

/// Team detail viewed by someone other than its owner lights Standings,
/// not My Team.
#[tokio::test]
async fn team_detail_lights_standings_for_a_non_owner_viewer() {
    let app = TestApp::new().await;
    let gen_league = factories::league(&app.pool, LeagueOptions::default()).await;
    let gen_team = factories::team(
        &app.pool,
        TeamOptions {
            league: Some(gen_league.league.clone()),
            ..Default::default()
        },
    )
    .await;
    app.login_as(&gen_league.commish).await;

    let resp = app.get(&format!("/teams/{}", gen_team.team.id)).await;
    resp.assert_status_ok();
    let body = resp.text();
    assert_eq!(
        body.matches(ACTIVE).count(),
        1,
        "exactly one tab should be active: {body}"
    );
    assert_eq!(
        href_before(&body, ACTIVE),
        "/standings",
        "a non-owner viewer should light Standings, not My Team: {body}"
    );
    assert!(
        body.contains(&as_rendered(&gen_team.team.name)),
        "bar should carry the team name: {body}"
    );
}

/// The team page titles its bar with the league name, so the league-name
/// subtitle rendered for Standings-lit pages must not repeat it below.
#[tokio::test]
async fn team_page_does_not_repeat_the_league_name_as_a_subtitle() {
    let app = TestApp::new().await;
    let gen_league = factories::league(&app.pool, LeagueOptions::default()).await;
    let gen_team = factories::team(
        &app.pool,
        TeamOptions {
            league: Some(gen_league.league.clone()),
            ..Default::default()
        },
    )
    .await;
    app.login_as(&gen_league.commish).await;

    let resp = app.get(&format!("/teams/{}", gen_team.team.id)).await;
    resp.assert_status_ok();
    let body = resp.text();
    let league = as_rendered(&gen_league.league.name);
    let header = &body[..body.find("</header>").expect("header closes")];
    assert!(
        header.contains(&league),
        "the bar should title the league: {header}"
    );
    assert!(
        !header.contains(&format!(r#"text-base-content/50">{league}"#)),
        "the bar already titles the league; the subtitle must not repeat it: {header}"
    );
}

/// Flash toasts and the bottom spacer must clear the fixed tab bar,
/// including the device safe-area inset the bar pads itself with. Focused
/// flows have no tab bar, so their toasts keep the plain bottom margin.
#[tokio::test]
async fn flash_toasts_and_spacer_clear_the_tab_bar() {
    let app = TestApp::new().await;
    let gen_league = factories::league(&app.pool, LeagueOptions::default()).await;
    app.login_as(&gen_league.commish).await;

    fn flash_class(body: &str) -> &str {
        let (_, rest) = body.split_once(r#"id="flash""#).expect("flash renders");
        let (tag, _) = rest.split_once('>').expect("flash tag closes");
        tag
    }

    let tabbed = app.get("/standings").await.text();
    assert!(
        flash_class(&tabbed).contains("safe-area-inset-bottom"),
        "tabbed pages must lift toasts above the tab bar: {tabbed}"
    );
    assert!(
        tabbed.contains("h-[calc(5rem+env(safe-area-inset-bottom))]"),
        "the spacer must reserve the bar's safe-area height: {tabbed}"
    );

    let focused = app
        .get(&format!("/teams/{}/edit", gen_league.commish_team.id))
        .await
        .text();
    assert!(
        !flash_class(&focused).contains("safe-area-inset-bottom"),
        "focused flows have no tab bar to clear: {focused}"
    );
}

/// Pages outside the four tabs (admin, invites) still show the tab bar, but
/// with nothing lit.
#[tokio::test]
async fn unlit_pages_show_the_tab_bar_with_nothing_lit() {
    let app = TestApp::new().await;
    let admin = factories::user(
        &app.pool,
        UserOptions {
            is_admin: true,
            ..Default::default()
        },
    )
    .await;
    factories::team(
        &app.pool,
        TeamOptions {
            owner: Some(admin.user.clone()),
            is_commish: true,
            ..Default::default()
        },
    )
    .await;
    app.login_as(&admin.user).await;

    for path in ["/invites", "/admin"] {
        let resp = app.get(path).await;
        resp.assert_status_ok();
        let body = resp.text();
        assert!(
            body.contains(r#"aria-label="Primary""#),
            "{path}: the tab bar should still show: {body}"
        );
        assert_eq!(
            body.matches(ACTIVE).count(),
            0,
            "{path}: no tab should be marked active: {body}"
        );
    }
}

/// Focused flows (✕, centered title, no tab bar) close to their canonical
/// parent.
#[tokio::test]
async fn focused_pages_close_to_their_canonical_parent() {
    let app = TestApp::new().await;
    let gen_league = factories::league(&app.pool, LeagueOptions::default()).await;
    app.login_as(&gen_league.commish).await;
    let team_path = format!("/teams/{}", gen_league.commish_team.id);

    for (path, parent) in [
        (format!("{team_path}/edit"), team_path.clone()),
        (format!("{team_path}/logo"), team_path.clone()),
        ("/invites/new".to_string(), "/invites".to_string()),
    ] {
        let resp = app.get(&path).await;
        resp.assert_status_ok();
        let body = resp.text();
        assert!(
            body.contains(&format!(
                r#"href="{parent}" class="btn btn-ghost btn-square -ml-2" aria-label="Close""#
            )),
            "{path}: should close to its canonical parent {parent}: {body}"
        );
        assert!(
            !body.contains(r#"aria-label="Primary""#),
            "{path}: a focused flow should not show the tab bar: {body}"
        );
    }
}

/// Every admin sub-page closes to its canonical parent; the admin index
/// itself shows the tab bar with nothing lit, and no close button.
#[tokio::test]
async fn admin_subtree_closes_to_canonical_parent() {
    let app = TestApp::new().await;
    let admin = factories::user(
        &app.pool,
        UserOptions {
            is_admin: true,
            ..Default::default()
        },
    )
    .await;
    factories::team(
        &app.pool,
        TeamOptions {
            owner: Some(admin.user.clone()),
            ..Default::default()
        },
    )
    .await;
    app.login_as(&admin.user).await;

    for (path, parent) in [
        ("/admin/contests", "/admin"),
        ("/admin/contests/setup", "/admin/contests"),
        ("/admin/salary-imports", "/admin"),
        ("/admin/avatars", "/admin"),
    ] {
        let resp = app.get(path).await;
        resp.assert_status_ok();
        let body = resp.text();
        assert!(
            body.contains(&format!(
                r#"href="{parent}" class="btn btn-ghost btn-square -ml-2" aria-label="Close""#
            )),
            "{path} should close to {parent}: {body}"
        );
    }

    let resp = app.get("/admin").await;
    resp.assert_status_ok();
    let body = resp.text();
    assert!(
        body.contains(r#"aria-label="Primary""#),
        "/admin is unlit but still shows the tab bar: {body}"
    );
    assert!(
        !body.contains(r#"aria-label="Close""#),
        "/admin is top-level and shows no close button: {body}"
    );
}

/// The Invite action lives in the bar, and the page no longer repeats its
/// title as an in-content heading.
#[tokio::test]
async fn invites_action_moves_to_navbar_end() {
    let app = TestApp::new().await;
    let gen_league = factories::league(&app.pool, LeagueOptions::default()).await;
    app.login_as(&gen_league.commish).await;

    let resp = app.get("/invites").await;
    resp.assert_status_ok();
    let body = resp.text();

    let navbar = body
        .split_once("navbar-end")
        .map(|(_, rest)| rest)
        .expect("page should render the navbar");
    let bar_end = &navbar[..navbar.find("</header>").expect("header closes")];
    assert!(
        bar_end.contains(r#"href="/invites/new""#),
        "Invite action should sit in navbar_end: {body}"
    );
    assert!(
        !body.contains("<h1"),
        "bar title replaces the in-content h1: {body}"
    );
}

/// The overflow menu's item labels, in order — every icon is followed by
/// the item's text, so the label is what runs from `</i>` to the next tag.
fn overflow_menu_items(html: &str) -> Vec<String> {
    let menu = html
        .split_once(r#"class="dropdown-content menu"#)
        .map(|(_, rest)| rest)
        .expect("overflow menu renders")
        .split_once("</ul>")
        .map(|(before, _)| before)
        .expect("overflow menu closes");
    menu.split("</i>")
        .skip(1)
        .filter_map(|rest| rest.split('<').next())
        .map(|label| label.trim().to_string())
        .filter(|label| !label.is_empty())
        .collect()
}

/// The overflow menu hangs off a More item at the far right of the tab
/// bar, opening upward; the top bar carries no menu of its own.
#[tokio::test]
async fn overflow_menu_lives_in_the_tab_bar() {
    let app = TestApp::new().await;
    let gen_league = factories::league(&app.pool, LeagueOptions::default()).await;
    app.login_as(&gen_league.commish).await;

    let resp = app.get("/standings").await;
    resp.assert_status_ok();
    let body = resp.text();

    let header_end = body.find("</header>").expect("header closes");
    assert!(
        !body[..header_end].contains("dropdown"),
        "the top bar should no longer carry the overflow menu: {body}"
    );

    let tab_bar = body
        .split_once(r#"aria-label="Primary""#)
        .map(|(_, rest)| rest)
        .expect("tab bar renders");
    assert!(
        tab_bar.contains("dropdown dropdown-top"),
        "More should open the menu upward from the tab bar: {body}"
    );
    assert!(
        tab_bar.contains(r#"<details class="dropdown dropdown-top"#),
        "More should use a native details toggle: {body}"
    );
    assert!(
        tab_bar.contains("</i>More"),
        "the menu trigger should read More, styled like a tab: {body}"
    );
    assert!(
        tab_bar.contains(r#"class="dropdown-content menu"#),
        "the menu itself should sit inside the tab bar: {body}"
    );
}

/// A single-league commissioner (not an admin) sees Invites, Rules & Scoring
/// and Sign out in the overflow menu — no league switcher (one league) and no
/// Admin Tools.
#[tokio::test]
async fn overflow_menu_lists_items_gated_by_role() {
    let app = TestApp::new().await;
    let gen_league = factories::league(&app.pool, LeagueOptions::default()).await;
    app.login_as(&gen_league.commish).await;

    let resp = app.get("/standings").await;
    resp.assert_status_ok();
    let body = resp.text();
    assert_eq!(
        overflow_menu_items(&body),
        ["Invites", "Rules &amp; Scoring", "Sign out"],
        "the overflow menu's items, in order: {body}"
    );
}
