use std::sync::{Arc, Mutex};

use crate::Config;
use lettre::AsyncTransport;

pub mod capture;
pub mod render;
pub mod transport;

/// A fully rendered email, ready for delivery or capture.
#[derive(Debug, Clone)]
pub struct OutgoingEmail {
    pub from: String,
    pub to: String,
    pub subject: String,
    pub html: String,
    pub text: String,
}

/// Errors during mail operations.
#[derive(Debug, thiserror::Error)]
pub enum MailError {
    #[error("template render failed: {0}")]
    Render(#[from] askama::Error),

    #[error("invalid email address: {0}")]
    Address(#[from] lettre::address::AddressError),

    #[error("failed to build TLS parameters: {0}")]
    Tls(lettre::transport::smtp::Error),

    #[error("failed to build email: {0}")]
    Build(#[from] lettre::error::Error),

    #[error("SMTP send failed: {0}")]
    Send(#[from] lettre::transport::smtp::Error),
}

/// Delivery mechanism. Cloneable so it can live in `AppState`.
#[derive(Clone)]
pub enum Mailer {
    Smtp {
        transport: lettre::AsyncSmtpTransport<lettre::Tokio1Executor>,
        default_from: String,
    },
    Capture(Arc<Mutex<Vec<OutgoingEmail>>>, String),
}

impl Mailer {
    /// Capture mailer with the given `From:` and an empty store.
    /// Clones share the underlying store.
    pub fn capture(from: impl Into<String>) -> Self {
        Mailer::Capture(Arc::new(Mutex::new(Vec::new())), from.into())
    }

    /// SMTP when `[smtp]` is present, otherwise capture for dev/test.
    pub fn from_config(config: &Config) -> Result<Self, MailError> {
        let mailer = match &config.smtp {
            Some(smtp) => Mailer::Smtp {
                transport: transport::build_transport(smtp)?,
                default_from: config.mail_from.to_owned(),
            },
            None => Mailer::capture(config.mail_from.to_owned()),
        };

        Ok(mailer)
    }

    /// The default `From:` address for outgoing emails.
    pub fn from(&self) -> &str {
        match self {
            Mailer::Smtp { default_from, .. } => default_from,
            Mailer::Capture(_, from) => from,
        }
    }

    /// Deliver an already-rendered email.
    ///
    /// Capture pushes synchronously; SMTP awaits delivery.
    /// Callers that must not block (HTTP handlers) should spawn.
    pub async fn send(&self, email: OutgoingEmail) -> Result<(), MailError> {
        match self {
            Mailer::Smtp { transport, .. } => {
                let from_mbox: lettre::message::Mailbox = email.from.parse()?;
                let to_mbox: lettre::message::Mailbox = email.to.parse()?;
                let msg = lettre::Message::builder()
                    .from(from_mbox)
                    .to(to_mbox)
                    .subject(&email.subject)
                    .multipart(
                        lettre::message::MultiPart::alternative()
                            .singlepart(
                                lettre::message::SinglePart::builder()
                                    .header(lettre::message::header::ContentType::TEXT_PLAIN)
                                    .body(email.text),
                            )
                            .singlepart(
                                lettre::message::SinglePart::builder()
                                    .header(lettre::message::header::ContentType::TEXT_HTML)
                                    .body(email.html),
                            ),
                    )?;
                transport.send(msg).await?;
                tracing::info!(to = %email.to, subject = %email.subject, "email sent");
                Ok(())
            }
            Mailer::Capture(store, _) => {
                store.lock().unwrap().push(email);
                Ok(())
            }
        }
    }

    /// All captured emails in insertion order.
    pub fn all(&self) -> Vec<OutgoingEmail> {
        match self {
            Mailer::Capture(store, _) => {
                let store = store.lock().unwrap();
                store.clone()
            }
            Mailer::Smtp { .. } => vec![],
        }
    }

    /// Get a single captured email by index.
    pub fn get(&self, idx: usize) -> Option<OutgoingEmail> {
        match self {
            Mailer::Capture(store, _) => store.lock().unwrap().get(idx).cloned(),
            Mailer::Smtp { .. } => None,
        }
    }

    /// Clear the capture store.
    pub fn clear(&self) {
        match self {
            Mailer::Capture(store, _) => store.lock().unwrap().clear(),
            Mailer::Smtp { .. } => (),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_send_stores_email_synchronously() {
        let mailer = Mailer::capture("test@example.com");

        let email = OutgoingEmail {
            from: "from@example.com".to_string(),
            to: "to@example.com".to_string(),
            subject: "Test".to_string(),
            html: "<p>Hello</p>".to_string(),
            text: "Hello".to_string(),
        };

        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(mailer.send(email))
            .expect("send should succeed");

        let stored = mailer.all();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].to, "to@example.com");
        assert_eq!(stored[0].subject, "Test");
        assert!(stored[0].html.contains("Hello"));
    }

    #[test]
    fn capture_clear_empties_store() {
        let mailer = Mailer::capture("test@example.com");

        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(mailer.send(OutgoingEmail {
            from: "from@example.com".to_string(),
            to: "to@example.com".to_string(),
            subject: "Test".to_string(),
            html: "".to_string(),
            text: "".to_string(),
        }))
        .expect("send");

        assert_eq!(mailer.all().len(), 1);
        mailer.clear();
        assert_eq!(mailer.all().len(), 0);
    }

    #[test]
    fn capture_get_returns_email_by_index() {
        let mailer = Mailer::capture("test@example.com");

        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(mailer.send(OutgoingEmail {
            from: "a@x.com".to_string(),
            to: "b@x.com".to_string(),
            subject: "first".to_string(),
            html: "".to_string(),
            text: "".to_string(),
        }))
        .expect("send 1");
        rt.block_on(mailer.send(OutgoingEmail {
            from: "c@x.com".to_string(),
            to: "d@x.com".to_string(),
            subject: "second".to_string(),
            html: "".to_string(),
            text: "".to_string(),
        }))
        .expect("send 2");

        assert_eq!(mailer.get(0).unwrap().subject, "first");
        assert_eq!(mailer.get(1).unwrap().subject, "second");
        assert!(mailer.get(2).is_none());
    }
}
