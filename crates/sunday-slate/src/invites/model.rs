use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use time::PrimitiveDateTime;

/// A row for the commissioner's invite list.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct PendingInvite {
    pub id: i64,
    pub email: String,
    pub expires_at: PrimitiveDateTime,
    pub accepted_at: Option<PrimitiveDateTime>,
}

/// A freshly minted accept link and how it was delivered.
///
/// The secret behind a link is hashed at rest, so a link exists only in the
/// response that mints it. The delivery outcome decides what the page tells
/// the commissioner.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum InviteLink {
    /// Handed back as the invite was created. The email carries this same
    /// link, so showing it retires nothing.
    Issued(String),
    /// Minted on demand for an invite that already existed. The stored secret
    /// could not be read back, so this necessarily replaced whatever link went
    /// out before and sent the new link by email.
    Reissued(String),
    /// Minted on demand, but email delivery failed. The commissioner can send
    /// this replacement link by hand.
    ReissuedEmailFailed(String),
}

impl InviteLink {
    pub fn url(&self) -> &str {
        match self {
            Self::Issued(url) | Self::Reissued(url) | Self::ReissuedEmailFailed(url) => url,
        }
    }

    /// True when showing this link stopped an earlier one from working.
    pub fn retires_previous(&self) -> bool {
        matches!(self, Self::Reissued(_) | Self::ReissuedEmailFailed(_))
    }

    pub fn email_failed(&self) -> bool {
        matches!(self, Self::ReissuedEmailFailed(_))
    }
}

/// A row of the commissioner's invite list, plus the accept link when one was
/// just minted for it. `link` is `None` on every row except the one the
/// commissioner just created or asked to copy.
pub struct InviteRow {
    pub invite: PendingInvite,
    pub link: Option<InviteLink>,
}

/// An invite token in URL form: `"{id}.{secret}"`. Same shape as `ResetToken`,
/// but resolves to a league + email rather than a user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InviteToken {
    pub id: i64,
    pub secret: String,
}

impl fmt::Display for InviteToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.id, self.secret)
    }
}

impl FromStr for InviteToken {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (id, secret) = s.split_once('.').ok_or(())?;
        let id: i64 = id.parse().map_err(|_| ())?;
        if secret.is_empty() {
            return Err(());
        }
        Ok(InviteToken {
            id,
            secret: secret.to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_then_parse_roundtrips() {
        let token = InviteToken {
            id: 7,
            secret: "ab-12".to_string(),
        };
        let parsed: InviteToken = token.to_string().parse().expect("roundtrip");
        assert_eq!(parsed, token);
    }

    #[test]
    fn parse_rejects_missing_dot() {
        assert!("nodot".parse::<InviteToken>().is_err());
    }

    #[test]
    fn parse_rejects_non_numeric_id() {
        assert!("abc.secret".parse::<InviteToken>().is_err());
    }

    #[test]
    fn parse_rejects_empty_secret() {
        assert!("7.".parse::<InviteToken>().is_err());
    }
}
