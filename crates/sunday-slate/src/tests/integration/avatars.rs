use sqlx::SqlitePool;

use crate::media::store as media_store;
use crate::tests::TestApp;
use crate::tests::factories::{self, UserOptions};
use crate::tests::utils::attr_before;

#[sqlx::test]
async fn admin_fetches_missing_images_from_rendered_links(pool: SqlitePool) {
    let app = TestApp::from_pool(pool.clone()).await;
    let admin = factories::user(
        &pool,
        UserOptions {
            is_admin: true,
            ..Default::default()
        },
    )
    .await;
    app.login_as(&admin.user).await;

    let dummy_media = media_store::insert(&pool, "journey-dummy.png", "image/png", 1, 1, 1)
        .await
        .expect("dummy media");
    sqlx::query("UPDATE nfl_teams SET logo_media_id = ?1")
        .bind(dummy_media)
        .execute(&pool)
        .await
        .expect("link teams");
    let admin_body = app.get("/admin").await.text();
    let player_images_href = attr_before(&admin_body, "href", "Player Images");
    assert_eq!(player_images_href, "/admin/avatars");

    let images_body = app.get(&player_images_href).await.text();
    let action = attr_before(&images_body, "action", "Fetch missing images");
    assert_eq!(action, "/admin/avatars/fetch");

    let response = app.post(&action, "").await;
    response.assert_status_ok();

    app.state.avatar_runner.wait_idle().await;

    let result = app.get("/admin/avatars").await;
    result.assert_status_ok();
    let body = result.text();
    assert!(body.contains("Fetched 0 headshots and 0 logos. 0 failed, 0 have no source."));
}
