use askama::Template;

use crate::mail::{MailError, OutgoingEmail};

#[derive(Template)]
#[template(path = "emails/password_reset.html")]
struct PasswordResetHtml<'a> {
    reset_link: &'a str,
}

#[derive(Template)]
#[template(path = "emails/password_reset.txt")]
struct PasswordResetText<'a> {
    reset_link: &'a str,
}

/// Render the password-reset email (HTML + text alternatives).
pub fn password_reset_email(
    from: &str,
    to: &str,
    subject: &str,
    reset_link: &str,
) -> Result<OutgoingEmail, MailError> {
    let html = PasswordResetHtml { reset_link }.render()?;
    let text = PasswordResetText { reset_link }.render()?;

    Ok(OutgoingEmail {
        from: from.to_string(),
        to: to.to_string(),
        subject: subject.to_string(),
        html,
        text,
    })
}

#[derive(Template)]
#[template(path = "emails/invite.html")]
struct InviteHtml<'a> {
    accept_link: &'a str,
    league_name: &'a str,
}

#[derive(Template)]
#[template(path = "emails/invite.txt")]
struct InviteText<'a> {
    accept_link: &'a str,
    league_name: &'a str,
}

/// Render the invite email (HTML + text alternatives).
pub fn invite_email(
    from: &str,
    to: &str,
    subject: &str,
    accept_link: &str,
    league_name: &str,
) -> Result<OutgoingEmail, MailError> {
    let html = InviteHtml {
        accept_link,
        league_name,
    }
    .render()?;
    let text = InviteText {
        accept_link,
        league_name,
    }
    .render()?;

    Ok(OutgoingEmail {
        from: from.to_string(),
        to: to.to_string(),
        subject: subject.to_string(),
        html,
        text,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_reset_email_renders_both_parts() {
        let link = "http://localhost:3000/reset-password?token=42.abc";
        let email = password_reset_email("from@x.com", "to@x.com", "Reset", link).expect("renders");
        assert!(
            email.html.contains(link),
            "html missing link: {}",
            email.html
        );
        assert!(
            email.text.contains(link),
            "text missing link: {}",
            email.text
        );
        assert!(
            email.html.contains("Reset your password"),
            "html missing heading: {}",
            email.html
        );
    }

    #[test]
    fn invite_email_states_the_fourteen_day_window() {
        let email = invite_email(
            "from@x.com",
            "to@x.com",
            "Invite",
            "http://localhost:3000/invite?token=7.abc",
            "Sunday Funday",
        )
        .expect("renders");
        assert!(
            email.html.contains("expires in 14 days"),
            "html missing expiry window: {}",
            email.html
        );
    }

    #[test]
    fn invite_email_renders_both_parts() {
        let link = "http://localhost:3000/invite?token=7.abc";
        let email = invite_email("from@x.com", "to@x.com", "Invite", link, "Sunday Funday")
            .expect("renders");
        assert!(
            email.html.contains(link),
            "html missing link: {}",
            email.html
        );
        assert!(
            email.text.contains(link),
            "text missing link: {}",
            email.text
        );
        assert!(
            email.html.contains("Sunday Funday"),
            "html missing league: {}",
            email.html
        );
    }
}
