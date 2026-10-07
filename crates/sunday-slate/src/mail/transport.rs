use utils::Secret;

use lettre::{
    AsyncSmtpTransport, Tokio1Executor,
    transport::smtp::{
        authentication::Credentials,
        client::{Tls, TlsParameters},
    },
};

fn default_smtp_port() -> u16 {
    587
}

/// SMTP config.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct SmtpConfig {
    pub host: String,

    #[serde(default = "default_smtp_port")]
    pub port: u16,

    pub username: String,

    pub password: Secret,
}

/// Build a lettre `AsyncSmtpTransport` from config.
pub fn build_transport(
    config: &SmtpConfig,
) -> Result<AsyncSmtpTransport<Tokio1Executor>, crate::mail::MailError> {
    let creds = Credentials::new(
        config.username.clone(),
        config.password.expose().to_string(),
    );

    let tls_params = TlsParameters::builder(config.host.clone())
        .dangerous_accept_invalid_certs(false)
        .build_rustls()
        .map_err(crate::mail::MailError::Tls)?;

    let tls = match config.port {
        465 => Tls::Wrapper(tls_params),
        _ => Tls::Required(tls_params),
    };

    Ok(AsyncSmtpTransport::<Tokio1Executor>::relay(&config.host)?
        .port(config.port)
        .credentials(creds)
        .tls(tls)
        .build())
}
