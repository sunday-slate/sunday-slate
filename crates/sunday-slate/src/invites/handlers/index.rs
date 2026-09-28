use crate::auth::Commissioner;
use crate::chrome::Chrome;
use crate::invites::model::{InviteLink, InviteRow};
use crate::invites::store;
use crate::leagues::CurrentLeague;
use crate::{AppError, AppState, Db};
use askama::Template;
use axum::extract::State;
use axum::response::{Html, IntoResponse};
use tower_sessions::Session;

#[derive(Template)]
#[template(path = "invites/index.html")]
pub struct InvitesIndexTemplate {
    pub rows: Vec<InviteRow>,
    pub chrome: Chrome,
}

impl InvitesIndexTemplate {
    pub fn new(rows: Vec<InviteRow>) -> Self {
        Self {
            rows,
            chrome: Chrome::unlit("Invites"),
        }
    }
}

/// The league's invites as list rows. `link_for` attaches a freshly minted
/// accept link to one of them; every other row carries none.
pub async fn rows_for(
    db: &Db,
    league_id: i64,
    link_for: Option<(i64, InviteLink)>,
) -> Result<Vec<InviteRow>, AppError> {
    let invites = store::list_for_league(db.reader(), league_id).await?;
    Ok(invites
        .into_iter()
        .map(|invite| {
            let link = link_for
                .as_ref()
                .filter(|(id, _)| *id == invite.id)
                .map(|(_, link)| link.clone());
            InviteRow { invite, link }
        })
        .collect())
}

pub async fn index(
    _: Commissioner,
    CurrentLeague(league): CurrentLeague,
    session: Session,
    State(state): State<AppState>,
) -> Result<impl IntoResponse, AppError> {
    let fresh = crate::invites::take_fresh_link(&session).await;
    let rows = rows_for(&state.db, league.id, fresh).await?;
    Ok(Html(InvitesIndexTemplate::new(rows).render()?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::invites::model::PendingInvite;
    use crate::tests::factories::TeamOptions;

    use crate::tests::{TestApp, factories};
    use http::StatusCode;
    use time::macros::datetime;

    fn pending(id: i64) -> PendingInvite {
        let ts = datetime!(2026-09-05 12:00:00);
        PendingInvite {
            id,
            email: "friend@example.com".to_string(),
            expires_at: ts,
            accepted_at: None,
        }
    }

    #[tokio::test]
    async fn non_commissioner_cannot_view_invites() {
        let app = TestApp::new().await;
        let gen_team = factories::team(&app.pool, TeamOptions::default()).await;

        app.login_as(&gen_team.owner).await;

        let resp = app.get("/invites").await;
        resp.assert_status(StatusCode::FORBIDDEN);
    }

    #[test]
    fn templates_render() {
        InvitesIndexTemplate::new(vec![])
            .render()
            .expect("empty index");

        let listed = InvitesIndexTemplate::new(vec![InviteRow {
            invite: pending(7),
            link: None,
        }])
        .render()
        .expect("index with a pending invite");
        assert!(
            listed.contains(r#"hx-post="/invites/7/link""#),
            "pending rows offer a copy-link button: {listed}"
        );
        assert!(
            !listed.contains("Only this link works now"),
            "no link is shown until one is asked for: {listed}"
        );
        assert!(
            listed.contains("list-col-grow"),
            "the email column must claim the row's slack: {listed}"
        );
    }

    /// `.list-row` is a grid whose default columns size the first child to its
    /// content. A full-width link input has to sit on its own wrapped row, or
    /// it stretches the column and pushes the buttons off.
    #[test]
    fn a_shown_link_wraps_onto_its_own_row() {
        let issued = InvitesIndexTemplate::new(vec![InviteRow {
            invite: pending(7),
            link: Some(InviteLink::Issued("http://x/invite?token=7.s".to_string())),
        }])
        .render()
        .expect("index with an issued link");
        assert!(
            issued.contains("list-col-wrap"),
            "the link block must wrap to its own grid row: {issued}"
        );
        assert!(
            issued.contains("Same link we emailed them."),
            "an issued link retires nothing: {issued}"
        );

        let reissued = InvitesIndexTemplate::new(vec![InviteRow {
            invite: pending(7),
            link: Some(InviteLink::Reissued(
                "http://x/invite?token=7.s".to_string(),
            )),
        }])
        .render()
        .expect("index with a reissued link");
        assert!(
            reissued.contains("Only this link works now"),
            "a reissued link says the earlier one stopped: {reissued}"
        );
    }
}
