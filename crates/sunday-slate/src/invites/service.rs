use uuid::Uuid;

use crate::auth::password;
use crate::invites::model::{InviteLink, InviteToken};
use crate::invites::store;
use crate::leagues::League;
use crate::mail::Mailer;
use crate::{AppError, Db};

/// A fresh secret and the hash to store for it. Only the secret leaves this
/// module — the invite row keeps the hash, so a link is unrecoverable once the
/// caller drops it.
fn mint_secret() -> (String, String) {
    let secret = Uuid::new_v4().to_string();
    let hash = password::hash(&secret);
    (secret, hash)
}

/// The URL a visitor follows to accept an invite.
fn accept_link(base_url: &str, token: &InviteToken) -> String {
    format!("{base_url}/invite?token={token}")
}
async fn send_invite_email(
    mailer: &Mailer,
    email: &str,
    league: &League,
    link: &str,
) -> Result<(), AppError> {
    let message = crate::mail::render::invite_email(
        mailer.from(),
        email,
        &format!("You're invited to {}", league.name),
        link,
        &league.name,
    )?;
    mailer.send(message).await?;
    Ok(())
}

/// A newly created invite: its row id, and the accept link that just went out
/// by email. The link is returned rather than discarded so the commissioner can
/// be shown it immediately — reading it back later is impossible.
pub struct SentInvite {
    pub id: i64,
    pub link: String,
}

/// Mint an invite token for `email` in `league`, persist it, and send the email.
/// Awaited (not spawned): the commissioner gets synchronous confirmation, and the
/// new invite is durable before the handler redirects to the list.
pub async fn send_invite(
    db: &Db,
    mailer: &Mailer,
    base_url: &str,
    league: &League,
    email: &str,
) -> Result<SentInvite, AppError> {
    let (secret, token_hash) = mint_secret();
    let id = db
        .write_tx(async |conn| -> Result<i64, sqlx::Error> {
            store::upsert(conn, league.id, email, &token_hash).await
        })
        .await?;
    let token = InviteToken { id, secret };
    let link = accept_link(base_url, &token);

    send_invite_email(mailer, email, league, &link).await?;
    Ok(SentInvite { id, link })
}

/// Mint a fresh link for an outstanding invite, attempt to email it once, and
/// show it so the commissioner can also send it by hand — a text message, say.
///
/// The stored secret is hashed and cannot be read back, so "show me the link"
/// necessarily means "mint a new one": this retires whichever link went out
/// before and restarts the invite's expiry. Email delivery is best effort after
/// the commit: a failure still returns the fresh link. `None` when the invite
/// is already accepted, revoked, or belongs to another league.
pub async fn invite_link(
    db: &Db,
    mailer: &Mailer,
    base_url: &str,
    league: &League,
    invite_id: i64,
) -> Result<Option<InviteLink>, AppError> {
    let (secret, token_hash) = mint_secret();
    let league_id = league.id;
    let Some(email) = db
        .write_tx(async |conn| -> Result<Option<String>, sqlx::Error> {
            store::reissue(&mut *conn, invite_id, league_id, &token_hash).await
        })
        .await?
    else {
        return Ok(None);
    };

    let token = InviteToken {
        id: invite_id,
        secret,
    };
    let link = accept_link(base_url, &token);
    match send_invite_email(mailer, &email, league, &link).await {
        Ok(()) => Ok(Some(InviteLink::Reissued(link))),
        Err(err) => {
            tracing::warn!(invite_id, error = %err, "Failed to email regenerated invite");
            Ok(Some(InviteLink::ReissuedEmailFailed(link)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::leagues;
    use crate::tests::factories;
    use crate::tests::factories::InviteOptions;
    use sqlx::SqlitePool;

    #[sqlx::test]
    async fn send_invite_names_the_league_in_the_subject(pool: SqlitePool) {
        let league = leagues::store::create(&pool, "Sunday Funday")
            .await
            .expect("seed league");
        let mailer = Mailer::capture("Sunday Slate <no-reply@example.com>");
        let db = Db::test(pool.clone());

        send_invite(
            &db,
            &mailer,
            "http://localhost:3000",
            &league,
            "invitee@example.com",
        )
        .await
        .expect("send");

        let sent = mailer.all();
        assert_eq!(sent[0].subject, "You're invited to Sunday Funday");
    }

    #[sqlx::test]
    async fn invite_link_mints_a_link_that_works_and_retires_the_old_one(pool: SqlitePool) {
        let league = leagues::store::create(&pool, "Sunday Funday")
            .await
            .expect("seed league");
        let existing = factories::invite(
            &pool,
            InviteOptions {
                league: Some(league.clone()),
                ..Default::default()
            },
        )
        .await;
        let db = Db::test(pool.clone());
        let mailer = Mailer::capture("Sunday Slate <no-reply@example.com>");
        let outcome = invite_link(&db, &mailer, "http://localhost:3000", &league, existing.id)
            .await
            .expect("link")
            .expect("invite is outstanding");
        let link = outcome.url();

        let fresh = link
            .strip_prefix("http://localhost:3000/invite?token=")
            .expect("link shape");
        assert!(
            store::verify(&db, fresh).await.expect("verify").is_some(),
            "the copied link must accept the invite"
        );
        assert!(
            store::verify(&db, &existing.token())
                .await
                .expect("verify")
                .is_none(),
            "the previously sent link must stop working"
        );
        let sent = mailer.all();
        assert_eq!(sent.len(), 1, "regeneration sends one invite email");
        assert_eq!(sent[0].to, existing.email);
        assert!(sent[0].html.contains(link));
        assert!(sent[0].text.contains(link));
    }

    #[sqlx::test]
    async fn invite_link_declines_an_invite_from_another_league(pool: SqlitePool) {
        let mine = leagues::store::create(&pool, "Mine")
            .await
            .expect("seed league");
        let theirs = leagues::store::create(&pool, "Theirs")
            .await
            .expect("seed league");
        let existing = factories::invite(
            &pool,
            InviteOptions {
                league: Some(theirs),
                ..Default::default()
            },
        )
        .await;
        let db = Db::test(pool.clone());

        assert!(
            invite_link(
                &db,
                &Mailer::capture("Sunday Slate <no-reply@example.com>"),
                "http://localhost:3000",
                &mine,
                existing.id,
            )
            .await
            .expect("link")
            .is_none()
        );
    }

    #[sqlx::test]
    async fn send_invite_persists_and_captures_email(pool: SqlitePool) {
        let league = leagues::store::create(&pool, "Sunday Funday")
            .await
            .expect("seed league");
        let mailer = Mailer::capture("Sunday Slate <no-reply@example.com>");

        let db = Db::test(pool.clone());
        send_invite(
            &db,
            &mailer,
            "http://localhost:3000",
            &league,
            "Invitee@Example.com",
        )
        .await
        .expect("send");

        let email: String = sqlx::query_scalar("SELECT email FROM invites")
            .fetch_one(&pool)
            .await
            .expect("invite row");
        assert_eq!(email, "invitee@example.com");

        let sent = mailer.all();
        assert_eq!(sent.len(), 1);
        assert!(sent[0].html.contains("/invite?token="));
    }
}
