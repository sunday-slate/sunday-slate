use crate::{DfsPosition, NflData, Salary, SalaryError};

impl NflData {
    /// Seed accepted legacy-like prices for fixtures; available only with `test-support`.
    pub async fn seed_salaries_for_test(&self, salaries: &[Salary]) -> Result<(), SalaryError> {
        self.managed
            .write_tx(async |conn| {
                for salary in salaries {
                    let position = match salary.dfs_position {
                        DfsPosition::Qb => "QB",
                        DfsPosition::Rb => "RB",
                        DfsPosition::Wr => "WR",
                        DfsPosition::Te => "TE",
                        DfsPosition::Dst => "DST",
                    };
                    sqlx::query("INSERT INTO current_salaries(gsis_game_id, gsis_player_id, team_abbr, dfs_position, salary, provenance, import_id, source_row_id, created_at, updated_at) VALUES (?, ?, ?, ?, ?, 'legacy', NULL, NULL, datetime('now','subsec'), datetime('now','subsec'))")
                        .bind(&salary.gsis_game_id)
                        .bind(&salary.gsis_player_id)
                        .bind(&salary.team_abbr.0)
                        .bind(position)
                        .bind(salary.salary)
                        .execute(&mut *conn)
                        .await?;
                }
                Ok(())
            })
            .await
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
}
