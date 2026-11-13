//! Owned injury source state: the Tank01 poller, its report consumer, and the
//! latest accepted report the snapshot build annotates from.

use std::sync::{Arc, Mutex, Weak};

use nfl_model::{InjuryReport, ProviderOutcome, ProviderResponse};
use tank01_data::{InjuryPoller, Tank01Client};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::NflData;

/// Nothing here is public: injuries are an input to snapshot publication, not
/// a service callers can query or refresh directly.
#[derive(Default)]
pub(crate) struct InjuryState {
    /// `None` leaves the source disabled: no worker, no HTTP.
    pub(crate) poller: Option<InjuryPoller>,
    /// Receive end of the poller's reports, taken once by the consumer.
    reports: Mutex<Option<mpsc::UnboundedReceiver<ProviderResponse<InjuryReport>>>>,
    consumer: Mutex<Option<JoinHandle<()>>>,
    /// The last successfully accepted report; a failed build keeps it for the
    /// next attempt.
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

    /// Abort the report consumer; the poller aborts its own worker on drop.
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
    /// The latest accepted injury report, if any. Builds read this once.
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

    /// Spawn the report consumer; the receiver is taken on the first call, so
    /// later calls are no-ops.
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
        // Upgrade only while processing one outcome, like the sync worker.
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
            // The source has no list before its first publication: publish the
            // same state as an empty report.
            ProviderOutcome::Pregame { .. } => {
                self.accept_injury_report(InjuryReport {
                    report_date: None,
                    entries: Vec::new(),
                })
                .await;
            }
            ProviderOutcome::Error(error) => {
                tracing::warn!(
                    error = %error,
                    "tank01 injury poll failed; keeping the last report"
                );
            }
        }
    }

    /// Retain a successful source outcome and republish under the publication
    /// lock. A failed build leaves readers on the current snapshot and keeps
    /// the new input for a later attempt.
    async fn accept_injury_report(&self, report: InjuryReport) {
        let _publication = self.publication.lock().await;
        let report = Arc::new(report);
        *self
            .injuries
            .latest
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(Arc::clone(&report));
        if let Err(error) = self.build_and_publish().await {
            tracing::error!(
                error = %error,
                "injury snapshot update failed; keeping the last published snapshot"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nfl_model::{DfsPositionSlotType, EspnPlayerId, InjuryDesignation, InjuryEntry};
    use time::macros::datetime;

    use crate::salaries;
    use crate::store::player_link::{self, PlayerLink};
    use crate::{DfsPosition, Game, NflData, Player, Season, SeasonWeek, TeamAbbr, Week};
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

    #[tokio::test]
    async fn a_failed_build_keeps_the_publication_and_the_new_report() {
        let nfl = fixture().await;
        nfl.rebuild_snapshot().await.expect("initial rebuild");
        let published = nfl.snapshot();
        let slate_id = crate::slates::SlateId(
            i64::from(
                SeasonWeek {
                    season: SEASON,
                    week: Week(WEEK),
                }
                .encode(),
            ) * 10
                + 1,
        );
        let injury = |snapshot: &crate::Snapshot| {
            snapshot
                .player(&slate_id, &NflPlayerId("00-MACK".into()))
                .expect("mack is built")
                .injury
        };
        assert_eq!(injury(&published), None);

        // Break a table the build reads, then accept a new report.
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

        // Repairing the source lets the retained input publish.
        rename_player_links(&nfl, "player_links_broken", "player_links").await;
        nfl.rebuild_snapshot().await.expect("rebuild");
        let snapshot = nfl.snapshot();
        assert!(!Arc::ptr_eq(&published, &snapshot));
        assert_eq!(
            injury(&snapshot),
            Some(InjuryDesignation::Doubtful),
            "the retained report appears once the build succeeds"
        );
        assert_eq!(
            snapshot
                .salaried_players_for_position(
                    &slate_id,
                    &DfsPositionSlotType::Position(DfsPosition::Rb)
                )
                .first()
                .expect("the RB bucket carries mack")
                .injury,
            Some(InjuryDesignation::Doubtful)
        );
    }
}
