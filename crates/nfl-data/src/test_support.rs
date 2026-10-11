use crate::{DfsPosition, NflData, Salary, SalaryError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestSalaryImportReport {
    pub games: usize,
    pub salaries: usize,
    pub unmatched: Vec<String>,
    pub player_ids: Vec<(String, String)>,
}

impl NflData {
    pub async fn in_memory_for_test(season: u16) -> Result<Self, crate::NflDataConnectError> {
        Self::in_memory_with_season(crate::Season(season)).await
    }

    /// Seed accepted legacy-like prices for fixtures; available only with `test-support`.
    pub async fn seed_salaries_for_test(&self, salaries: &[Salary]) -> Result<(), SalaryError> {
        self.managed.write_tx(async |conn| {
            for salary in salaries {
                let position = match salary.dfs_position { DfsPosition::Qb => "QB", DfsPosition::Rb => "RB", DfsPosition::Wr => "WR", DfsPosition::Te => "TE", DfsPosition::Dst => "DST" };
                sqlx::query("INSERT INTO current_salaries(gsis_game_id, gsis_player_id, team_abbr, dfs_position, salary, provenance, import_id, source_row_id, created_at, updated_at) VALUES (?, ?, ?, ?, ?, 'legacy', NULL, NULL, datetime('now','subsec'), datetime('now','subsec'))")
                    .bind(&salary.gsis_game_id).bind(&salary.gsis_player_id).bind(&salary.team_abbr.0).bind(position).bind(salary.salary).execute(&mut *conn).await?;
            }
            Ok(())
        }).await
    }

    pub async fn import_salary_csv_for_test(
        &self,
        bytes: &[u8],
        filename: Option<&str>,
    ) -> Result<TestSalaryImportReport, SalaryError> {
        use crate::salary_imports::{
            model::RowState,
            service::{ImportError, SalaryImportService},
            store,
        };
        let service = SalaryImportService::new(self);
        let import_id = service
            .upload(bytes, filename)
            .await
            .map_err(|e| SalaryError::conflict(e.to_string()))?;
        let rows = store::rows_for(self.managed.reader(), import_id).await?;
        let unmatched: Vec<String> = rows
            .iter()
            .filter(|r| r.state == RowState::Unmatched)
            .map(|r| r.fd_name.clone())
            .collect();
        let player_ids = rows
            .iter()
            .filter_map(|r| {
                r.gsis_player_id
                    .as_ref()
                    .map(|gsis| (r.fd_player_id.clone(), gsis.clone()))
            })
            .collect();
        for row in rows.iter().filter(|r| r.state == RowState::Unmatched) {
            service
                .skip(import_id, row.id)
                .await
                .map_err(|e| SalaryError::conflict(e.to_string()))?;
        }
        let committed = service.commit(import_id).await.map_err(|e| match e {
            ImportError::Internal(message)
            | ImportError::Unavailable(message)
            | ImportError::InvalidInput(message) => SalaryError::conflict(message),
            other => SalaryError::conflict(other.to_string()),
        })?;
        Ok(TestSalaryImportReport {
            games: committed.games,
            salaries: committed.salaries,
            unmatched,
            player_ids,
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::{DfsPosition, NflData, Salary, TeamAbbr};

    #[tokio::test]
    async fn salary_test_seed_is_readable_from_managed_store() {
        let nfl = NflData::in_memory().await.unwrap();
        let salary = Salary {
            gsis_game_id: "2025_01_BUF_NYJ".into(),
            gsis_player_id: Some("00-test".into()),
            team_abbr: TeamAbbr("BUF".into()),
            dfs_position: DfsPosition::Qb,
            salary: 8200,
        };
        nfl.seed_salaries_for_test(std::slice::from_ref(&salary))
            .await
            .unwrap();
        assert_eq!(
            nfl.salaries_for_games(&["2025_01_BUF_NYJ"]).await.unwrap(),
            vec![salary]
        );
    }

    #[tokio::test]
    async fn salary_csv_helper_skips_unmatched_rows_and_uses_commit_path() {
        use crate::{Game, Player, Season, SeasonType, Week};
        use time::macros::datetime;
        let nfl = NflData::in_memory().await.unwrap();
        nfl.seed_for_test(
            &[Player {
                gsis_id: "00-josh".into(),
                espn_id: None,
                full_name: "Josh Allen".into(),
                first_name: Some("Josh".into()),
                last_name: Some("Allen".into()),
                position: Some("QB".into()),
                latest_team: Some(TeamAbbr("BUF".into())),
                headshot_url: None,
            }],
            &[Game {
                gsis_game_id: "2026_01_BUF_NYJ".into(),
                season: Season(2026),
                week: Week(1),
                season_type: SeasonType::Reg,
                kickoff: Some(datetime!(2026-09-10 00:00 UTC)),
                home_team: TeamAbbr("NYJ".into()),
                away_team: TeamAbbr("BUF".into()),
                home_score: None,
                away_score: None,
            }],
        )
        .await
        .unwrap();
        let csv = b"\"Id\",\"Position\",\"First Name\",\"Nickname\",\"Last Name\",\"FPPG\",\"Played\",\"Salary\",\"Game\",\"Team\",\"Opponent\",\"Injury Indicator\",\"Injury Details\"\n\"1-10\",\"QB\",\"Josh\",\"\",\"Allen\",\"0\",\"0\",\"8000\",\"BUF@NYJ\",\"BUF\",\"NYJ\",\"\",\"\"\n\"1-11\",\"WR\",\"No\",\"\",\"Name\",\"0\",\"0\",\"5000\",\"BUF@NYJ\",\"BUF\",\"NYJ\",\"\",\"\"\n";
        let report = nfl
            .import_salary_csv_for_test(csv, Some("fixture.csv"))
            .await
            .unwrap();
        assert_eq!(report.games, 1);
        assert_eq!(report.salaries, 1);
        assert_eq!(report.unmatched, ["No Name"]);
        let prices = nfl.salaries_for_games(&["2026_01_BUF_NYJ"]).await.unwrap();
        assert_eq!(prices.len(), 1);
        assert_eq!(prices[0].gsis_player_id.as_deref(), Some("00-josh"));
    }
}
