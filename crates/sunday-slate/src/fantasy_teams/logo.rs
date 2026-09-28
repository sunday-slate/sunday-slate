use crate::fantasy_teams::FantasyTeam;
use crate::fantasy_teams::store as fantasy_teams;
use crate::media::{self, Media};
use crate::{AppError, Db};
use std::path::Path;

/// Set `team`'s logo from an uploaded image: replace an existing logo in
/// place (same media row + file), otherwise create and link a media row.
/// Files are named `<uuid>.png` so media URLs can't be enumerated.
///
/// The FK is re-read inside the write transaction — the request's `team`
/// snapshot can lose a race with a concurrent upload/remove for the same
/// team. A failed file write rolls the row back; the old file is untouched.
pub async fn upload_logo(
    db: &Db,
    media_dir: &Path,
    team: &FantasyTeam,
    bytes: &[u8],
) -> Result<(), AppError> {
    let png =
        media::process::process_logo(bytes).map_err(|e| AppError::BadRequest(e.to_string()))?;

    db.write_tx(async |conn| -> Result<(), AppError> {
        let existing = fantasy_teams::logo_media_id(&mut *conn, team.id).await?;
        let id = media::save_png(&mut *conn, media_dir, existing, None, &png, 256, 256).await?;
        if existing.is_none() {
            fantasy_teams::set_logo_media_id(&mut *conn, team.id, id).await?;
        }
        Ok(())
    })
    .await
}

/// Remove `team`'s logo: clear the FK, delete the media row, then delete the
/// file once the transaction has committed. The FK is re-read inside the
/// transaction for the same race reason as `upload_logo`.
pub async fn remove_logo(db: &Db, media_dir: &Path, team: &FantasyTeam) -> Result<(), AppError> {
    let removed = db
        .write_tx(async |conn| -> Result<Option<Media>, AppError> {
            let Some(id) = fantasy_teams::logo_media_id(&mut *conn, team.id).await? else {
                return Ok(None);
            };
            let current = media::store::get(&mut *conn, id)
                .await?
                .expect("fantasy_teams.logo_media_id FK references media");
            fantasy_teams::clear_logo(&mut *conn, team.id).await?;
            media::store::delete(&mut *conn, id).await?;
            Ok(Some(current))
        })
        .await?;
    if let Some(media) = removed {
        let _ = tokio::fs::remove_file(media_dir.join(&media.path)).await;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::TestApp;
    use crate::tests::factories::{self, TeamOptions};
    use crate::tests::utils::png_bytes;

    #[tokio::test]
    async fn upload_then_remove_roundtrips_file_and_fk() {
        let app = TestApp::new().await;
        let gen_team = factories::team(&app.pool, TeamOptions::default()).await;
        let state = app.state();

        upload_logo(
            &state.db,
            &state.config.media_dir,
            &gen_team.team,
            &png_bytes(300, 200),
        )
        .await
        .expect("upload");

        // Reload team; it now references a media row, and the file exists.
        let team = fantasy_teams::team_for_user(&app.pool, gen_team.league.id, gen_team.owner.id)
            .await
            .expect("team")
            .expect("some");
        let media_id = team.logo_media_id.expect("logo set");
        let media = media::store::get(&app.pool, media_id)
            .await
            .expect("get")
            .expect("media row");
        let stem = media.path.strip_suffix(".png").expect("png name");
        assert!(
            uuid::Uuid::parse_str(stem).is_ok(),
            "unguessable file name, got: {}",
            media.path
        );
        let file = state.config.media_dir.join(&media.path);
        assert!(file.exists(), "logo file written");

        remove_logo(&state.db, &state.config.media_dir, &team)
            .await
            .expect("remove");
        let team = fantasy_teams::team_for_user(&app.pool, gen_team.league.id, gen_team.owner.id)
            .await
            .expect("team")
            .expect("some");
        assert!(team.logo_media_id.is_none(), "fk cleared");
        assert!(!file.exists(), "logo file removed");
    }

    #[tokio::test]
    async fn upload_twice_keeps_one_media_row() {
        let app = TestApp::new().await;
        let gen_team = factories::team(&app.pool, TeamOptions::default()).await;
        let state = app.state();

        upload_logo(
            &state.db,
            &state.config.media_dir,
            &gen_team.team,
            &png_bytes(300, 200),
        )
        .await
        .expect("first");

        let first_path: String = sqlx::query_scalar("SELECT path FROM media")
            .fetch_one(&app.pool)
            .await
            .expect("path");

        // The same pre-upload snapshot (logo_media_id = None): the service
        // re-reads inside the tx, so this replaces instead of inserting a
        // racing second row.
        upload_logo(
            &state.db,
            &state.config.media_dir,
            &gen_team.team,
            &png_bytes(100, 400),
        )
        .await
        .expect("second");

        let paths: Vec<String> = sqlx::query_scalar("SELECT path FROM media")
            .fetch_all(&app.pool)
            .await
            .expect("paths");
        assert_eq!(
            paths,
            vec![first_path],
            "replace updates in place: one row, same file"
        );
    }

    #[tokio::test]
    async fn upload_rejects_non_image() {
        let app = TestApp::new().await;
        let gen_team = factories::team(&app.pool, TeamOptions::default()).await;
        let state = app.state();
        let err = upload_logo(&state.db, &state.config.media_dir, &gen_team.team, b"nope")
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::BadRequest(_)));
    }
}
