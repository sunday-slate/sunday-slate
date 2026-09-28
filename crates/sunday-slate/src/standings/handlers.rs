use askama::Template;
use axum::extract::State;
use axum::response::Html;

use crate::chrome::{Chrome, Tab};
use crate::fantasy_teams::{FantasyTeamId, MaybeTeam};
use crate::leagues::CurrentLeague;
use crate::standings::service;
use crate::standings::standings::StandingRow;
use crate::{AppError, AppState};

#[derive(Template)]
#[template(path = "standings/index.html")]
struct StandingsPage {
    chrome: Chrome,
    rows: Vec<StandingRow>,
    current_team: Option<FantasyTeamId>,
}

impl StandingsPage {
    fn is_current_team(&self, row: &StandingRow) -> bool {
        self.current_team == Some(row.fantasy_team)
    }
}

pub async fn index(
    State(state): State<AppState>,
    CurrentLeague(league): CurrentLeague,
    MaybeTeam(team): MaybeTeam,
) -> Result<Html<String>, AppError> {
    let rows = service::standings_for_league(&state, league.id).await?;
    let current_team = team.map(|team| FantasyTeamId(team.id));
    Ok(Html(
        StandingsPage {
            chrome: Chrome::tabbed("Standings", Tab::Standings),
            rows,
            current_team,
        }
        .render()?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::standings::standings::StandingRow;

    fn row(name: &str, rank: Option<u32>, total: Option<f64>, wins: u32) -> StandingRow {
        StandingRow {
            fantasy_team: FantasyTeamId(1),
            rank,
            name: name.to_string(),
            owner_name: "Mike".to_string(),
            logo: None,
            total,
            wins,
        }
    }

    #[test]
    fn renders_rank_total_and_owner() {
        let html = StandingsPage {
            chrome: Chrome::tabbed("Sunday Funday", Tab::Standings),
            rows: vec![
                row("Gridiron Giants", Some(1), Some(1247.5), 3),
                row("Lone Victors", Some(2), Some(900.0), 1),
            ],
            current_team: None,
        }
        .render()
        .expect("render");
        assert!(html.contains("Gridiron Giants"));
        assert!(html.contains("Mike"));
        assert!(html.contains("1,247.50"));
        assert!(html.contains("3 wins"), "wins shown when > 0");
        assert!(html.contains("1 win<"), "singular at exactly one: {html}");
        assert!(!html.contains("bg-primary/10"), "no current team highlight");
        assert!(
            !html.contains("hover:bg-primary/20"),
            "no current team hover highlight"
        );
    }

    #[test]
    fn renders_logo_image_instead_of_monogram_when_present() {
        let mut with_logo = row("Gridiron Giants", Some(1), Some(1247.5), 0);
        with_logo.logo = Some("/media/7.png?v=1".to_string());
        let html = StandingsPage {
            chrome: Chrome::tabbed("Sunday Funday", Tab::Standings),
            rows: vec![with_logo],
            current_team: None,
        }
        .render()
        .expect("render");
        assert!(
            html.contains(r#"src="/media/7.png?v=1""#),
            "logo img: {html}"
        );
        assert!(!html.contains("GG"), "no monogram fallback: {html}");
    }

    #[test]
    fn hides_zero_wins_and_blank_total() {
        let html = StandingsPage {
            chrome: Chrome::tabbed("Sunday Funday", Tab::Standings),
            rows: vec![row("Bench Warmers", None, None, 0)],
            current_team: None,
        }
        .render()
        .expect("render");
        assert!(html.contains("Bench Warmers"));
        assert!(!html.contains("wins"), "no wins figure at 0: {html}");
        assert!(!html.contains("0.00"), "no zero-filler total");
    }

    #[test]
    fn rows_link_to_team_detail() {
        let html = StandingsPage {
            chrome: Chrome::tabbed("Sunday Funday", Tab::Standings),
            rows: vec![row("Gridiron Giants", Some(1), Some(1247.5), 0)],
            current_team: None,
        }
        .render()
        .expect("render");
        assert!(html.contains(r#"href="/teams/1""#), "row links: {html}");
    }
}
