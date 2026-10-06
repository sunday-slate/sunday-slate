use askama::Template;
use nfl_data::DatasetFreshness;
use time::OffsetDateTime;

use crate::background::{LastRunExt, LastRunView, format_timestamp};
use crate::chrome::Chrome;
use crate::web::FormView;
use crate::{AppError, AppState};
use utils::background::{LastRun, RunnerState};

#[derive(Template)]
#[template(path = "admin/nfl_sync.html", blocks = ["panel"])]
pub(super) struct NflSyncTemplate {
    pub(super) chrome: Chrome,
    pub(super) running: bool,
    pub(super) elapsed: String,
    pub(super) rows: Vec<DatasetRow>,
    pub(super) last_run: Option<LastRunView>,
    pub(super) scheduler_line: Option<String>,
}

impl FormView for NflSyncTemplate {
    fn render_form(&self) -> Result<String, askama::Error> {
        self.as_panel().render()
    }
}

pub(super) struct DatasetRow {
    pub(super) dataset: &'static str,
    pub(super) assets: u32,
    pub(super) last_synced_at: Option<String>,
}

impl NflSyncTemplate {
    pub(super) async fn load(state: &AppState) -> Result<Self, AppError> {
        if let RunnerState::Running {
            started_at,
            progress: freshness,
        } = state.sync_runner.snapshot()
        {
            return Ok(Self::running(started_at, freshness));
        }
        let freshness = state.nfl.freshness().await?;
        match state.sync_runner.snapshot() {
            RunnerState::Idle { last } => {
                let scheduler_line = state.config.nfl_sync_interval().map(|interval| {
                    format!(
                        "Automatic refresh {}.",
                        crate::nfl_sync::interval_label(interval.as_secs())
                    )
                });
                Ok(Self::idle(freshness, last, scheduler_line))
            }
            RunnerState::Running {
                started_at,
                progress: freshness,
            } => Ok(Self::running(started_at, freshness)),
        }
    }

    fn idle(
        freshness: Vec<DatasetFreshness>,
        last: Option<LastRun<String>>,
        scheduler_line: Option<String>,
    ) -> Self {
        Self {
            chrome: chrome(),
            running: false,
            elapsed: String::new(),
            rows: dataset_rows(freshness),
            last_run: last.map(|last| last.view(|text| text)),
            scheduler_line,
        }
    }

    fn running(started_at: OffsetDateTime, freshness: Vec<DatasetFreshness>) -> Self {
        let seconds = (OffsetDateTime::now_utc() - started_at)
            .whole_seconds()
            .max(0);
        Self {
            chrome: chrome(),
            running: true,
            elapsed: format!("{}:{:02}", seconds / 60, seconds % 60),
            rows: dataset_rows(freshness),
            last_run: None,
            scheduler_line: None,
        }
    }
}

fn chrome() -> Chrome {
    Chrome::focused("NFL data sync", "/admin")
}

fn dataset_rows(freshness: Vec<DatasetFreshness>) -> Vec<DatasetRow> {
    freshness
        .into_iter()
        .map(|row| DatasetRow {
            dataset: row.dataset.name(),
            assets: row.assets,
            last_synced_at: row.last_synced_at.map(format_timestamp),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use askama::Template;
    use nfl_data::{Dataset, DatasetFreshness};
    use time::OffsetDateTime;
    use time::macros::datetime;

    use super::NflSyncTemplate;
    use utils::background::LastRun;

    fn freshness() -> Vec<DatasetFreshness> {
        vec![DatasetFreshness {
            dataset: Dataset::Schedules,
            assets: 2,
            last_synced_at: Some(datetime!(2026-08-20 12:00:00 UTC)),
        }]
    }

    #[test]
    fn idle_renders_page_and_panel() {
        let template = NflSyncTemplate::idle(freshness(), None, None);
        let page = template.render().expect("full page");
        let panel = template.as_panel().render().expect("panel");

        assert!(page.contains("<!doctype html>"));
        assert!(page.contains(r#"method="post" action="/admin/nfl-sync""#));
        assert!(page.contains("2026-08-20T12:00:00Z"));
        assert!(!page.contains("hx-trigger"));
        assert!(!page.contains("Automatic refresh"));
        assert!(panel.contains(r#"id="nfl-sync-panel""#));
        assert!(!panel.contains("<!doctype html>"));
    }

    #[test]
    fn running_panel_polls_and_disables_the_button() {
        let template = NflSyncTemplate::running(OffsetDateTime::now_utc(), freshness());
        let panel = template.as_panel().render().expect("panel");

        assert!(panel.contains(r#"hx-trigger="every 3s""#));
        assert!(panel.contains("loading-spinner"));
        assert!(panel.contains("disabled>Sync now"));
        assert!(!panel.contains("<form"));
        assert!(!panel.contains("Automatic refresh"));
    }

    #[test]
    fn failed_last_run_renders_an_error_alert() {
        let last = LastRun {
            finished_at: datetime!(2026-08-20 12:05:00 UTC),
            outcome: Err("players: FAILED: boom".to_string()),
        };
        let panel = NflSyncTemplate::idle(freshness(), Some(last), None)
            .as_panel()
            .render()
            .expect("panel");

        assert!(panel.contains("alert-error"));
        assert!(panel.contains("Last sync failed"));
        assert!(panel.contains("2026-08-20T12:05:00Z"));
        assert!(panel.contains("players: FAILED: boom"));
    }

    #[test]
    fn scheduler_line_renders_when_configured() {
        let template = NflSyncTemplate::idle(
            freshness(),
            None,
            Some("Automatic refresh every hour.".into()),
        );
        let page = template.render().expect("full page");

        assert!(page.contains("Automatic refresh every hour."));
    }
}
