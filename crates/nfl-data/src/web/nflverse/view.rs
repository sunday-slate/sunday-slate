use askama::Template;
use nflverse_data::DatasetFreshness;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use utils::background::{LastRun, RunnerState};

use crate::{NflData, web::error::AdminError};

#[derive(Template)]
#[template(
    path = "admin/nflverse.html",
    config = "src/web/askama.toml",
    blocks = ["panel"]
)]
pub(crate) struct NflverseTemplate {
    running: bool,
    elapsed: String,
    rows: Vec<DatasetRow>,
    last_run: Option<LastRunView>,
    scheduler_line: Option<String>,
}

impl NflverseTemplate {
    pub(crate) async fn load(nfl: &NflData) -> Result<Self, AdminError> {
        let status = nfl.provider.refresh_status();
        if let RunnerState::Running {
            started_at,
            progress,
        } = status
        {
            return Ok(Self::running(started_at, progress));
        }

        // Recheck state after freshness I/O so a newly-started refresh is not
        // knowingly rendered as idle.
        let freshness = nfl.provider.freshness().await?;
        match nfl.provider.refresh_status() {
            RunnerState::Idle { last } => Ok(Self::idle(
                freshness,
                last,
                nfl.refresh_interval.map(|interval| {
                    format!("Automatic refresh {}.", interval_label(interval.as_secs()))
                }),
            )),
            RunnerState::Running {
                started_at,
                progress,
            } => Ok(Self::running(started_at, progress)),
        }
    }

    fn idle(
        freshness: Vec<DatasetFreshness>,
        last: Option<LastRun<String>>,
        scheduler_line: Option<String>,
    ) -> Self {
        Self {
            running: false,
            elapsed: String::new(),
            rows: dataset_rows(freshness),
            last_run: last.map(last_run_view),
            scheduler_line,
        }
    }

    fn running(started_at: OffsetDateTime, freshness: Vec<DatasetFreshness>) -> Self {
        let seconds = (OffsetDateTime::now_utc() - started_at)
            .whole_seconds()
            .max(0);
        Self {
            running: true,
            elapsed: format!("{}:{:02}", seconds / 60, seconds % 60),
            rows: dataset_rows(freshness),
            last_run: None,
            scheduler_line: None,
        }
    }
}

struct DatasetRow {
    dataset: &'static str,
    assets: u32,
    last_synced_at: Option<String>,
}

struct LastRunView {
    ok: bool,
    finished_at: String,
    text: String,
}

fn last_run_view(last: LastRun<String>) -> LastRunView {
    LastRunView {
        ok: last.outcome.is_ok(),
        finished_at: format_timestamp(last.finished_at),
        text: last.outcome.unwrap_or_else(|error| error),
    }
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

pub(super) fn format_timestamp(value: OffsetDateTime) -> String {
    value
        .to_offset(time::UtcOffset::UTC)
        .replace_nanosecond(0)
        .expect("zero nanoseconds is valid")
        .format(&Rfc3339)
        .unwrap_or_default()
}

pub(super) fn interval_label(secs: u64) -> String {
    let (amount, unit) = if secs.is_multiple_of(3600) {
        (secs / 3600, "hour")
    } else if secs.is_multiple_of(60) {
        (secs / 60, "minute")
    } else {
        return format!("every {secs} seconds");
    };
    if amount == 1 {
        format!("every {unit}")
    } else {
        format!("every {amount} {unit}s")
    }
}

#[cfg(test)]
mod tests {
    use super::{NflverseTemplate, format_timestamp, interval_label};
    use crate::test::test_support::{mock_nfl, mock_nfl_delayed};
    use askama::Template;
    use std::time::Duration;
    use time::{OffsetDateTime, macros::datetime};

    #[test]
    fn cadence_and_timestamp_formatting_are_stable() {
        assert_eq!(interval_label(3600), "every hour");
        assert_eq!(interval_label(90), "every 90 seconds");
        assert_eq!(
            format_timestamp(datetime!(2026-08-20 12:00:00.456 UTC)),
            "2026-08-20T12:00:00Z"
        );
        assert!(OffsetDateTime::now_utc().unix_timestamp() > 0);
    }

    #[tokio::test]
    async fn nflverse_idle_scheduler_and_last_run_presentation() {
        let (nfl, _, _) = mock_nfl(Some(Duration::from_secs(3600))).await;
        let page = NflverseTemplate::load(&nfl)
            .await
            .unwrap()
            .render()
            .unwrap();
        assert!(page.contains("Automatic refresh every hour."));
        assert_eq!(page.matches("Never synced").count(), 5);

        let (nfl, _, _) = mock_nfl(None).await;
        assert!(
            !NflverseTemplate::load(&nfl)
                .await
                .unwrap()
                .render()
                .unwrap()
                .contains("Automatic refresh")
        );
    }

    #[tokio::test]
    async fn nflverse_running_panel_polls_and_disables_submission() {
        let (nfl, _, _) = mock_nfl_delayed(Duration::from_millis(800)).await;
        assert!(nfl.provider.request_refresh().await.unwrap());
        assert!(!nfl.provider.request_refresh().await.unwrap());
        let template = NflverseTemplate::load(&nfl).await.unwrap();
        let panel = template.as_panel().render().unwrap();
        assert!(panel.contains("hx-trigger=\"every 3s\""));
        assert!(panel.contains("/nfl-data-admin/nflverse"));
        assert!(panel.contains("loading-spinner"));
        assert!(panel.contains("Running for"));
        assert!(panel.contains("disabled>Sync now"));
        assert!(!panel.contains("<form"));
        assert!(!panel.contains("Automatic refresh"));
    }

    #[tokio::test]
    async fn nflverse_failed_and_partial_syncs_show_reports() {
        use utils::background::RunnerState;

        for succeeds in [false, true] {
            let (nfl, _server, _cache) =
                crate::test::test_support::mock_nfl_with_schedule(None, succeeds).await;
            assert!(nfl.provider.request_refresh().await.unwrap());
            let mut completed = false;
            for _ in 0..200 {
                if matches!(
                    nfl.provider.refresh_status(),
                    RunnerState::Idle { last: Some(_) }
                ) {
                    completed = true;
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            assert!(completed, "refresh completed");
            let page = NflverseTemplate::load(&nfl)
                .await
                .unwrap()
                .render()
                .unwrap();
            assert!(page.contains("Last sync failed"));
            if succeeds {
                assert!(
                    !nfl.provider
                        .games(crate::Season(2025))
                        .await
                        .unwrap()
                        .is_empty()
                );
            }
        }
    }

    #[tokio::test]
    async fn nflverse_report_text_is_escaped() {
        let (nfl, _, _) = mock_nfl(None).await;
        let template = NflverseTemplate::idle(
            nfl.provider.freshness().await.unwrap(),
            Some(utils::background::LastRun {
                finished_at: datetime!(2026-08-20 12:05:00 UTC),
                outcome: Err("<script>alert(1)</script>".to_string()),
            }),
            None,
        );
        let panel = template.as_panel().render().unwrap();
        assert!(!panel.contains("<script>alert(1)</script>"));
        assert!(panel.contains("&#60;script&#62;"), "{panel}");
    }
}
