use askama::Template;
use time::OffsetDateTime;

use crate::avatars::{FetchProgress, FetchReport};
use crate::background::{LastRun, LastRunView, RunnerState};
use crate::chrome::Chrome;
use crate::nfl_players::store as player_store;
use crate::nfl_teams::store as team_store;
use crate::web::FormView;
use crate::{AppError, AppState};

#[derive(Template)]
#[template(path = "admin/avatars/index.html", blocks = ["panel"])]
pub(super) struct AvatarsTemplate {
    pub(super) chrome: Chrome,
    pub(super) running: bool,
    pub(super) elapsed: String,
    pub(super) progress: FetchProgress,
    pub(super) missing_headshots: usize,
    pub(super) missing_logos: usize,
    pub(super) last_run: Option<LastRunView>,
}

impl FormView for AvatarsTemplate {
    fn render_form(&self) -> Result<String, askama::Error> {
        self.as_panel().render()
    }
}

impl AvatarsTemplate {
    pub(super) async fn load(state: &AppState) -> Result<Self, AppError> {
        if let RunnerState::Running {
            started_at,
            progress,
        } = state.avatar_runner.snapshot()
        {
            return Ok(Self::running(started_at, progress));
        }
        let missing_headshots =
            player_store::count_missing_headshots(state.db.reader()).await? as usize;
        let missing_logos = team_store::count_missing_logos(state.db.reader()).await? as usize;
        match state.avatar_runner.snapshot() {
            RunnerState::Idle { last } => Ok(Self::idle(missing_headshots, missing_logos, last)),
            RunnerState::Running {
                started_at,
                progress,
            } => Ok(Self::running(started_at, progress)),
        }
    }

    fn idle(
        missing_headshots: usize,
        missing_logos: usize,
        last: Option<LastRun<FetchReport>>,
    ) -> Self {
        Self {
            chrome: chrome(),
            running: false,
            elapsed: String::new(),
            progress: FetchProgress::default(),
            missing_headshots,
            missing_logos,
            last_run: last.map(|last| last.view(|report| report_text(&report))),
        }
    }

    fn running(started_at: OffsetDateTime, progress: FetchProgress) -> Self {
        let seconds = (OffsetDateTime::now_utc() - started_at)
            .whole_seconds()
            .max(0);
        Self {
            chrome: chrome(),
            running: true,
            elapsed: format!("{}:{:02}", seconds / 60, seconds % 60),
            progress,
            missing_headshots: 0,
            missing_logos: 0,
            last_run: None,
        }
    }
}

pub(super) fn report_text(report: &FetchReport) -> String {
    format!(
        "Fetched {} headshots and {} logos. {} failed, {} have no source.",
        report.players_fetched, report.teams_fetched, report.failed, report.skipped
    )
}

fn chrome() -> Chrome {
    Chrome::focused("Player Images", "/admin")
}

#[cfg(test)]
mod tests {
    use askama::Template;
    use time::OffsetDateTime;
    use time::macros::datetime;

    use super::*;

    #[test]
    fn idle_renders_counts_button_and_no_poll() {
        let template = AvatarsTemplate::idle(3, 1, None);
        let page = template.render().expect("page");
        let panel = template.as_panel().render().expect("panel");
        assert!(page.contains("<!doctype html>"));
        assert!(page.contains(r#"action="/admin/avatars/fetch""#));
        assert!(page.contains(">3</dd>"));
        assert!(page.contains(">1</dd>"));
        assert!(!page.contains("hx-trigger"));
        assert!(panel.contains(r#"id="avatars-panel""#));
        assert!(!panel.contains("<!doctype html>"));
    }

    #[test]
    fn running_panel_polls_and_shows_progress() {
        let progress = FetchProgress {
            total: 941,
            fetched: 312,
            failed: 2,
            skipped: 35,
        };
        let panel = AvatarsTemplate::running(OffsetDateTime::now_utc(), progress)
            .as_panel()
            .render()
            .expect("panel");
        assert!(
            panel.contains(r#"hx-get="/admin/avatars" hx-trigger="every 3s" hx-swap="outerHTML""#)
        );
        assert!(panel.contains("loading-spinner"));
        assert!(panel.contains("Fetched 312 of 941 · 2 failed · 35 no source"));
        assert!(panel.contains("disabled>Fetch missing images"));
        assert!(!panel.contains("<form"));
    }

    #[test]
    fn last_run_success_and_failure_render_alerts() {
        let ok = LastRun {
            finished_at: datetime!(2026-08-24 12:05:00 UTC),
            outcome: Ok(FetchReport {
                players_fetched: 909,
                teams_fetched: 32,
                failed: 0,
                skipped: 35,
            }),
        };
        let panel = AvatarsTemplate::idle(0, 0, Some(ok))
            .as_panel()
            .render()
            .expect("panel");
        assert!(panel.contains("alert-success"));
        assert!(panel.contains("Fetched 909 headshots and 32 logos. 0 failed, 35 have no source."));
        assert!(panel.contains("2026-08-24T12:05:00Z"));

        let err = LastRun {
            finished_at: datetime!(2026-08-24 12:05:00 UTC),
            outcome: Err("database is locked".to_string()),
        };
        let panel = AvatarsTemplate::idle(0, 0, Some(err))
            .as_panel()
            .render()
            .expect("panel");
        assert!(panel.contains("alert-error"));
        assert!(panel.contains("database is locked"));
    }
}
