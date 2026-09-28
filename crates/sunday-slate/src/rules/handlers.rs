use askama::Template;
use axum::response::{Html, IntoResponse};

use crate::AppError;
use crate::chrome::Chrome;

#[derive(Template)]
#[template(path = "rules/index.html")]
struct IndexTemplate {
    chrome: Chrome,
}

pub async fn index() -> Result<impl IntoResponse, AppError> {
    Ok(Html(
        IndexTemplate {
            chrome: Chrome::unlit("Rules & Scoring"),
        }
        .render()?,
    ))
}

#[cfg(test)]
mod tests {
    use crate::tests::TestApp;
    use crate::tests::factories::{self, LeagueOptions};
    use http::StatusCode;

    async fn rules_page() -> String {
        let app = TestApp::new().await;
        let gen_league = factories::league(&app.pool, LeagueOptions::default()).await;
        app.login_as(&gen_league.commish).await;

        let resp = app.get("/rules").await;
        resp.assert_status_ok();
        resp.text()
    }

    #[tokio::test]
    async fn offense_scoring_lists_its_point_values() {
        let body = rules_page().await;

        for value in ["0.04", "0.1", "0.5", "-1", "-2"] {
            assert!(
                body.contains(value),
                "offense scoring should list {value}: {body}"
            );
        }
        assert!(
            body.contains("Passing yards") && body.contains("Receptions"),
            "each abbreviation should be spelled out: {body}"
        );
    }

    #[tokio::test]
    async fn offense_scoring_lists_the_yardage_bonuses() {
        let body = rules_page().await;

        for bonus in [
            "300+ passing yards",
            "100+ rushing yards",
            "100+ receiving yards",
        ] {
            assert!(
                body.contains(bonus),
                "should list the {bonus} bonus: {body}"
            );
        }
    }

    #[tokio::test]
    async fn defense_scoring_lists_the_points_allowed_tiers() {
        let body = rules_page().await;

        for tier in ["1–6", "7–13", "14–20", "21–27", "28–34", "35+"] {
            assert!(
                body.contains(tier),
                "should list the {tier} points-allowed tier: {body}"
            );
        }
    }

    #[tokio::test]
    async fn roster_section_states_the_slots_and_the_cap() {
        let body = rules_page().await;

        assert!(body.contains("$60,000"), "should state the cap: {body}");
        assert!(
            body.contains("RB / WR / TE"),
            "should say what FLEX accepts: {body}"
        );
        for slot in ["QB", "FLEX", "D/ST"] {
            assert!(body.contains(slot), "should list the {slot} slot: {body}");
        }
    }

    #[tokio::test]
    async fn contest_section_states_the_top_ten_season_total() {
        let body = rules_page().await;

        assert!(
            body.contains("top 10"),
            "the season total counts a team's top 10 scores: {body}"
        );
    }

    /// The page is league-private: a signed-out visitor is sent to login and
    /// returned to `/rules` afterward.
    #[tokio::test]
    async fn signed_out_visitor_is_sent_to_login() {
        let app = TestApp::new().await;
        factories::league(&app.pool, LeagueOptions::default()).await;

        let resp = app.get("/rules").await;

        resp.assert_status(StatusCode::TEMPORARY_REDIRECT);
        assert_eq!(resp.header("location"), "/login?next=%2Frules");
    }
}
