use std::fmt;
use std::str::FromStr;
use std::sync::LazyLock;

use uuid::Uuid;

use super::password;
use super::store;
use crate::mail::Mailer;
use crate::users::store as users;
use crate::{AppError, Db};

/// A password-reset token in URL form: `"{id}.{secret}"`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResetToken {
    pub id: i64,
    pub secret: String,
}

impl fmt::Display for ResetToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.id, self.secret)
    }
}

impl FromStr for ResetToken {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (id, secret) = s.split_once('.').ok_or(())?;
        let id: i64 = id.parse().map_err(|_| ())?;
        if secret.is_empty() {
            return Err(());
        }
        Ok(ResetToken {
            id,
            secret: secret.to_string(),
        })
    }
}

pub struct VerifiedToken {
    pub token_id: i64,
    pub user_id: i64,
}

/// Mint a reset token for `address` and email the reset link.
/// No-op when no account matches the address.
///
/// Pure orchestration over explicit dependencies — testable by calling directly
/// with a capture mailer. The forgot-password handler runs it on a detached task.
pub async fn send_reset_email(
    db: &Db,
    mailer: &Mailer,
    base_url: &str,
    address: &str,
) -> Result<(), AppError> {
    let Some(user) = users::find_by_email(db.reader(), address).await? else {
        return Ok(());
    };

    let secret = Uuid::new_v4().to_string();
    let token_hash = password::hash(&secret);
    let id = db
        .write_tx(async |conn| -> Result<i64, sqlx::Error> {
            store::insert_token(conn, user.id, &token_hash).await
        })
        .await?;
    let token = ResetToken { id, secret };

    let reset_link = format!("{base_url}/reset-password?token={token}");
    let email = crate::mail::render::password_reset_email(
        mailer.from(),
        &user.email,
        "Reset your Sunday Slate password",
        &reset_link,
    )?;
    mailer.send(email).await?;
    Ok(())
}

/// Dummy hash to keep timing uniform when no live token matches the selector.
static DUMMY_HASH: LazyLock<String> = LazyLock::new(|| password::hash("dummy"));

pub async fn verify(db: &Db, raw: &str) -> Result<Option<VerifiedToken>, AppError> {
    let parsed = raw.parse::<ResetToken>().ok();
    let row = match &parsed {
        Some(token) => store::find_active_token(db.reader(), token.id).await?,
        None => None,
    };

    // Constant-time: always one argon2 verify, even on miss.
    let secret = parsed.as_ref().map(|t| t.secret.as_str()).unwrap_or("");
    let stored_hash = row
        .as_ref()
        .map_or(DUMMY_HASH.as_str(), |r| r.token_hash.as_str());
    if password::verify(secret, stored_hash)
        && let Some(row) = row
    {
        return Ok(Some(VerifiedToken {
            token_id: row.id,
            user_id: row.user_id,
        }));
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::store as reset_tokens;
    use crate::tests::factories::UserOptions;
    use crate::tests::utils::*;
    use crate::tests::{TestApp, factories};
    use fake::Fake;
    use fake::faker::internet::en::SafeEmail;

    /// `send_reset_email` for a known user captures one email with the reset link.
    #[tokio::test]
    async fn send_reset_email_delivers_link_to_user() {
        let app = TestApp::new().await;
        let gen_user = factories::user(&app.pool, UserOptions::default()).await;
        let db = Db::test(app.pool.clone());

        send_reset_email(
            &db,
            &app.mailer,
            "http://localhost:3000",
            &gen_user.user.email,
        )
        .await
        .expect("send_reset_email");

        let emails = app.mailer.all();
        assert_eq!(emails.len(), 1, "exactly one email");
        assert_eq!(emails[0].to, gen_user.user.email.as_str());
        assert_eq!(emails[0].subject, "Reset your Sunday Slate password");
        // The href renders with `| safe`, so the link appears verbatim.
        assert!(
            emails[0]
                .html
                .contains("http://localhost:3000/reset-password?token="),
            "HTML body missing reset link: {}",
            emails[0].html
        );
        assert!(
            emails[0]
                .text
                .contains("http://localhost:3000/reset-password?token="),
            "text body missing reset link: {}",
            emails[0].text
        );

        let token_count = reset_tokens::token_count(&app.pool)
            .await
            .expect("token count");
        assert_eq!(token_count, 1);
    }

    /// `send_reset_email` matches email case-insensitively.
    #[tokio::test]
    async fn send_reset_email_matches_email_case_insensitively() {
        let app = TestApp::new().await;
        let gen_user = factories::user(&app.pool, UserOptions::default()).await;
        let db = Db::test(app.pool.clone());

        send_reset_email(
            &db,
            &app.mailer,
            "http://localhost:3000",
            &gen_user.user.email.to_uppercase(),
        )
        .await
        .expect("send_reset_email");

        let emails = app.mailer.all();
        assert_eq!(emails.len(), 1);
        assert_eq!(emails[0].to, gen_user.user.email.as_str());

        let token_count = reset_tokens::token_count(&app.pool)
            .await
            .expect("token count");
        assert_eq!(token_count, 1);
    }

    /// `send_reset_email` is a no-op for an unknown address: no email, no token.
    #[tokio::test]
    async fn send_reset_email_unknown_address_sends_nothing() {
        let app = TestApp::new().await;
        let db = Db::test(app.pool.clone());

        let email: String = SafeEmail().fake();

        send_reset_email(&db, &app.mailer, "http://localhost:3000", &email)
            .await
            .expect("send_reset_email is a no-op for unknown addresses");

        assert_eq!(app.mailer.all().len(), 0, "no email for unknown address");

        let token_count = reset_tokens::token_count(&app.pool)
            .await
            .expect("token count");
        assert_eq!(token_count, 0, "no token for unknown address");
    }

    /// Send failure surfaces as Err, but the token is committed first (retryable).
    #[tokio::test]
    async fn send_reset_email_reports_send_failure_after_committing_token() {
        let mailer = failing_mailer();
        let app = TestApp::new_with_mailer(mailer).await;
        let gen_user = factories::user(&app.pool, UserOptions::default()).await;
        let db = Db::test(app.pool.clone());

        let result = send_reset_email(
            &db,
            &app.mailer,
            "http://localhost:3000",
            &gen_user.user.email,
        )
        .await;

        assert!(
            result.is_err(),
            "a send failure should surface as Err for the caller to log"
        );

        let token_count = reset_tokens::token_count(&app.pool)
            .await
            .expect("token count");
        assert_eq!(token_count, 1, "token is committed before the failing send");
    }

    #[test]
    fn display_then_parse_roundtrips() {
        let token = ResetToken {
            id: 42,
            secret: "3f9a-c1".to_string(),
        };
        let parsed: ResetToken = token.to_string().parse().expect("roundtrip");
        assert_eq!(parsed, token);
    }

    #[test]
    fn parse_rejects_missing_dot() {
        assert!("nodothere".parse::<ResetToken>().is_err());
    }

    #[test]
    fn parse_rejects_non_numeric_id() {
        assert!("abc.secret".parse::<ResetToken>().is_err());
    }

    #[test]
    fn parse_rejects_empty_secret() {
        assert!("42.".parse::<ResetToken>().is_err());
    }
}
