use std::sync::{Arc, Mutex, Weak};

use nfl_model::{InjuryReport, ProviderOutcome, ProviderResponse};
use tank01_data::{InjuryPoller, Tank01Client};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::Instrument;

use crate::NflData;

#[derive(Default)]
pub(crate) struct InjuryState {
    pub(crate) poller: Option<InjuryPoller>,
    reports: Mutex<Option<mpsc::UnboundedReceiver<ProviderResponse<InjuryReport>>>>,
    consumer: Mutex<Option<JoinHandle<()>>>,
    // Retain successful input even when publication fails.
    latest: Mutex<Option<Arc<InjuryReport>>>,
}

impl InjuryState {
    pub(crate) fn configured(client: Tank01Client) -> Self {
        let (reports, receiver) = mpsc::unbounded_channel();
        Self {
            poller: Some(InjuryPoller::new(client, reports)),
            reports: Mutex::new(Some(receiver)),
            consumer: Mutex::new(None),
            latest: Mutex::new(None),
        }
    }

    pub(crate) fn shutdown(&mut self) {
        if let Some(worker) = self
            .consumer
            .get_mut()
            .unwrap_or_else(|error| error.into_inner())
            .take()
        {
            worker.abort();
        }
    }
}

impl NflData {
    pub(crate) fn injury_report(&self) -> Option<Arc<InjuryReport>> {
        self.injuries
            .latest
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    #[cfg(test)]
    pub(crate) fn retain_injury_report_for_test(&self, report: InjuryReport) {
        *self
            .injuries
            .latest
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(Arc::new(report));
    }

    pub(crate) fn start_injury_worker(self: &Arc<Self>) {
        let Some(mut reports) = self
            .injuries
            .reports
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
        else {
            return;
        };
        let updates = Arc::downgrade(self);
        let worker = tokio::spawn(async move {
            while let Some(response) = reports.recv().await {
                let Some(nfl) = Weak::upgrade(&updates) else {
                    return;
                };
                nfl.apply_injury_response(response).await;
            }
        });
        *self
            .injuries
            .consumer
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(worker);
    }

    pub(crate) fn start_injury_polling(&self) {
        if let Some(poller) = &self.injuries.poller {
            poller.start();
        }
    }

    pub(crate) fn stop_injury_polling(&self) {
        if let Some(poller) = &self.injuries.poller {
            poller.stop();
        }
    }

    async fn apply_injury_response(&self, response: ProviderResponse<InjuryReport>) {
        match response.outcome {
            ProviderOutcome::Value(report) => self.accept_injury_report(report).await,
            ProviderOutcome::Pregame { .. } => {
                self.accept_injury_report(InjuryReport {
                    report_date: None,
                    entries: Vec::new(),
                })
                .await;
            }
            ProviderOutcome::Error(error) => {
                tracing::debug!(error = %error, "injury update skipped; retaining the last report");
            }
        }
    }

    async fn accept_injury_report(&self, report: InjuryReport) {
        let span = tracing::info_span!(
            parent: None,
            "snapshot_update",
            season = self.season.0,
            trigger = "injury_report",
            entries = report.entries.len(),
            report_date = ?report.report_date,
        );
        async {
            tracing::info!("snapshot update started");
            let _publication = self.publication.lock().await;
            let report = Arc::new(report);
            *self
                .injuries
                .latest
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = Some(Arc::clone(&report));
            drop(self.build_and_publish().await);
        }
        .instrument(span)
        .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nfl_model::{EspnPlayerId, InjuryDesignation, InjuryEntry};
    use time::macros::datetime;

    use crate::salaries;
    use crate::store::player_link::{self, PlayerLink};
    use crate::{Game, NflData, Player, Season, SeasonWeek, TeamAbbr, Week};
    use nfl_model::NflPlayerId;

    const SEASON: Season = Season(2026);
    const WEEK: u8 = 5;

    fn report(designation: InjuryDesignation) -> InjuryReport {
        InjuryReport {
            report_date: None,
            entries: vec![InjuryEntry {
                espn_id: Some(EspnPlayerId("ESPN-MACK".into())),
                name: "Mack Runner".into(),
                team: None,
                position: None,
                designation,
            }],
        }
    }

    async fn fixture() -> NflData {
        let nfl = NflData::in_memory().await.expect("in-memory handle");
        let season_week = SeasonWeek {
            season: SEASON,
            week: Week(WEEK),
        };
        nfl.seed_for_test(
            &[Player {
                gsis_id: "00-MACK".into(),
                espn_id: Some("ESPN-MACK".into()),
                full_name: "Mack Runner".into(),
                first_name: None,
                last_name: None,
                position: Some("RB".into()),
                latest_team: Some(TeamAbbr::KC),
                headshot_url: None,
            }],
            &[Game {
                game_id: "2026_05_BUF_KC".into(),
                season_week,
                kickoff: Some(datetime!(2026-10-04 17:00 UTC)),
                home_team: TeamAbbr::BUF,
                away_team: TeamAbbr::KC,
                home_score: None,
                away_score: None,
            }],
        )
        .await
        .expect("seed nflverse");
        let csv = "\
Id,Position,First Name,Nickname,Last Name,FPPG,Played,Salary,Game,Team,Opponent,Injury Indicator,Injury Details,Tier,,,Roster Position
9001-1,RB,Mack,Mack Runner,Runner,,0,9000,KC@BUF,KC,BUF,,,,,,RB/FLEX
";
        let rows = salaries::parse_fanduel_csv(&nfl.fanduel, season_week, csv.as_bytes())
            .expect("parse salaries");
        salaries::save_fanduel_salaries(&nfl.fanduel, &rows)
            .await
            .expect("save salaries");
        player_link::save_links(
            &nfl.store,
            &[PlayerLink {
                fd_player_id: 1,
                gsis_player_id: "00-MACK".into(),
                espn_player_id: Some("ESPN-MACK".into()),
            }],
        )
        .await
        .expect("save link");
        crate::slates::populate_at(
            &nfl.nflverse,
            &nfl.store,
            SEASON,
            datetime!(2026-09-01 12:00 UTC),
        )
        .await
        .expect("populate slates");
        nfl
    }

    async fn rename_player_links(nfl: &NflData, from: &str, to: &str) {
        nfl.store
            .write_tx(async |conn| {
                sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
                    "ALTER TABLE {from} RENAME TO {to}"
                )))
                .execute(&mut *conn)
                .await?;
                Ok::<(), sqlx::Error>(())
            })
            .await
            .expect("rename table");
    }

    fn slate_id() -> crate::slates::SlateId {
        crate::slates::SlateId(
            i64::from(
                SeasonWeek {
                    season: SEASON,
                    week: Week(WEEK),
                }
                .encode(),
            ) * 10
                + 1,
        )
    }

    #[tokio::test]
    async fn a_failed_build_keeps_the_publication_and_the_new_report() {
        let nfl = fixture().await;
        nfl.rebuild_snapshot().await.expect("initial rebuild");
        let published = nfl.snapshot();
        let slate = slate_id();
        let injury = |snapshot: &crate::Snapshot| {
            snapshot
                .player(&slate, &NflPlayerId("00-MACK".into()))
                .expect("mack is built")
                .injury
        };
        assert_eq!(injury(&published), None);

        rename_player_links(&nfl, "player_links", "player_links_broken").await;
        nfl.accept_injury_report(report(InjuryDesignation::Doubtful))
            .await;
        assert!(
            Arc::ptr_eq(&published, &nfl.snapshot()),
            "the failed build left the publication alone"
        );
        assert_eq!(
            nfl.injury_report()
                .expect("the new report is retained")
                .entries[0]
                .designation,
            InjuryDesignation::Doubtful
        );

        rename_player_links(&nfl, "player_links_broken", "player_links").await;
        nfl.rebuild_snapshot().await.expect("rebuild");
        let snapshot = nfl.snapshot();
        assert!(!Arc::ptr_eq(&published, &snapshot));
        assert_eq!(
            injury(&snapshot),
            Some(InjuryDesignation::Doubtful),
            "the retained report appears once the build succeeds"
        );
    }
}
