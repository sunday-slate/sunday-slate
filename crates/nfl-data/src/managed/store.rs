use std::{str::FromStr, time::Duration};

use sqlx::{
    migrate::Migrator,
    sqlite::{
        SqliteConnectOptions, SqliteConnection, SqliteJournalMode, SqlitePool, SqlitePoolOptions,
        SqliteSynchronous,
    },
};

use super::{error::SalaryError, model::Salary};

static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

pub(crate) struct ManagedStore {
    reader: SqlitePool,
    writer: SqlitePool,
}

impl ManagedStore {
    pub(crate) async fn open(url: &str) -> Result<Self, SalaryError> {
        if url == "sqlite::memory:" {
            return Self::in_memory().await;
        }
        let options = SqliteConnectOptions::from_str(url)?
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .foreign_keys(true)
            .busy_timeout(Duration::from_secs(10))
            .optimize_on_close(true, None);
        let writer = SqlitePoolOptions::new()
            .max_connections(1)
            .acquire_timeout(Duration::from_secs(30))
            .connect_with(options.clone())
            .await?;
        let existing_tables: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'",
        )
        .fetch_one(&writer)
        .await?;
        let has_migration_table: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='_sqlx_migrations'",
        )
        .fetch_one(&writer)
        .await?;
        if existing_tables > 0 {
            if has_migration_table == 0 {
                return Err(SalaryError::unsupported_schema());
            }
            let compatible: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM _sqlx_migrations WHERE version = 20261008000000",
            )
            .fetch_one(&writer)
            .await?;
            if compatible == 0 {
                return Err(SalaryError::unsupported_schema());
            }
        }
        MIGRATOR.run(&writer).await?;
        let reader = SqlitePoolOptions::new()
            .max_connections(4)
            .acquire_timeout(Duration::from_secs(5))
            .connect_with(options.read_only(true))
            .await?;
        Ok(Self { reader, writer })
    }

    pub(crate) async fn in_memory() -> Result<Self, SalaryError> {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await?;
        MIGRATOR.run(&pool).await?;
        Ok(Self {
            reader: pool.clone(),
            writer: pool,
        })
    }

    #[allow(dead_code)] // Used by the private import/review services introduced next.
    pub(crate) fn reader(&self) -> &SqlitePool {
        &self.reader
    }

    pub(crate) async fn write_tx<F, T, E>(&self, f: F) -> Result<T, E>
    where
        F: for<'c> AsyncFnOnce(&'c mut SqliteConnection) -> Result<T, E>,
        E: From<sqlx::Error>,
    {
        let mut tx = self.writer.begin_with("BEGIN IMMEDIATE").await?;
        let result = f(&mut tx).await;
        match result {
            Ok(value) => {
                tx.commit().await?;
                Ok(value)
            }
            Err(error) => {
                tx.rollback().await?;
                Err(error)
            }
        }
    }

    #[allow(dead_code)] // Used by the private salary matcher introduced next.
    pub(crate) async fn mapping_by_fd_id(
        &self,
        fd_player_id: &str,
    ) -> Result<Option<String>, SalaryError> {
        Ok(sqlx::query_scalar(
            "SELECT gsis_player_id FROM confirmed_player_mappings WHERE fd_player_id = ?",
        )
        .bind(fd_player_id)
        .fetch_optional(&self.reader)
        .await?)
    }

    pub(crate) async fn salaries_for_games<I: AsRef<str>>(
        &self,
        ids: &[I],
    ) -> Result<Vec<Salary>, SalaryError> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let unique: std::collections::BTreeSet<&str> = ids.iter().map(AsRef::as_ref).collect();
        let mut query = sqlx::QueryBuilder::<sqlx::Sqlite>::new(
            "SELECT gsis_game_id, gsis_player_id, team_abbr, dfs_position, salary FROM current_salaries WHERE gsis_game_id IN (",
        );
        let mut separated = query.separated(", ");
        for id in unique {
            separated.push_bind(id);
        }
        separated.push_unseparated(
            ") ORDER BY gsis_game_id, salary DESC, COALESCE(gsis_player_id, team_abbr)",
        );
        let rows = query
            .build_query_as::<(String, Option<String>, String, String, i64)>()
            .fetch_all(&self.reader)
            .await?;
        rows.into_iter()
            .map(|(game, player, team, position, salary)| {
                let dfs_position = match position.as_str() {
                    "QB" => nfl_model::DfsPosition::Qb,
                    "RB" => nfl_model::DfsPosition::Rb,
                    "WR" => nfl_model::DfsPosition::Wr,
                    "TE" => nfl_model::DfsPosition::Te,
                    "DST" => nfl_model::DfsPosition::Dst,
                    _ => {
                        return Err(SalaryError::conflict(format!(
                            "unsupported stored DFS position {position}"
                        )));
                    }
                };
                Ok(Salary {
                    gsis_game_id: game,
                    gsis_player_id: player,
                    team_abbr: nfl_model::TeamAbbr(team),
                    dfs_position,
                    salary,
                })
            })
            .collect()
    }
}
