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

fn is_memory_database(url: &str, options: &SqliteConnectOptions) -> bool {
    let is_sqlx_memory = options
        .get_filename()
        .to_string_lossy()
        .starts_with("file:sqlx-in-memory-");
    let is_memory_file = options.get_filename() == std::path::Path::new(":memory:");
    let query = url
        .split_once('?')
        .map(|(_, query)| query)
        .unwrap_or_default();
    let is_memory_mode = url::form_urlencoded::parse(query.as_bytes())
        .any(|(key, value)| key == "mode" && value == "memory");
    is_sqlx_memory || is_memory_file || is_memory_mode
}
impl ManagedStore {
    pub(crate) async fn open(url: &str) -> Result<Self, SalaryError> {
        let options = SqliteConnectOptions::from_str(url)?
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .foreign_keys(true)
            .busy_timeout(Duration::from_secs(10))
            .optimize_on_close(true, None);
        let is_memory = is_memory_database(url, &options);
        let writer_options = if is_memory {
            Self::in_memory_pool_options()
        } else {
            SqlitePoolOptions::new()
                .max_connections(1)
                .acquire_timeout(Duration::from_secs(30))
        };
        let writer = writer_options.connect_with(options.clone()).await?;
        let existing_tables: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name NOT GLOB 'sqlite_*'",
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
            let (history_entries, compatible): (i64, i64) = sqlx::query_as(
                "SELECT COUNT(*), COUNT(CASE WHEN version = 20261008000000 THEN 1 END) FROM _sqlx_migrations",
            )
            .fetch_one(&writer)
            .await?;
            let only_empty_migration_history = existing_tables == 1 && history_entries == 0;
            if compatible == 0 && !only_empty_migration_history {
                return Err(SalaryError::unsupported_schema());
            }
        }
        MIGRATOR.run(&writer).await?;
        let reader = if is_memory {
            writer.clone()
        } else {
            SqlitePoolOptions::new()
                .max_connections(4)
                .acquire_timeout(Duration::from_secs(5))
                .connect_with(options.read_only(true))
                .await?
        };
        Ok(Self { reader, writer })
    }

    fn in_memory_pool_options() -> SqlitePoolOptions {
        SqlitePoolOptions::new()
            .max_connections(1)
            .idle_timeout(None)
            .max_lifetime(None)
            .test_before_acquire(false)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub(crate) async fn in_memory() -> Result<Self, SalaryError> {
        Self::open("sqlite::memory:").await
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

#[cfg(test)]
mod tests {
    use super::ManagedStore;
    use sqlx::sqlite::SqlitePoolOptions;

    #[test]
    fn in_memory_pool_disables_retirement_and_acquisition_pings() {
        let options = ManagedStore::in_memory_pool_options();
        assert_eq!(options.get_max_connections(), 1);
        assert_eq!(options.get_idle_timeout(), None);
        assert_eq!(options.get_max_lifetime(), None);
        assert!(!options.get_test_before_acquire());
    }

    #[tokio::test]
    async fn memory_url_spellings_keep_managed_schema_and_salary() {
        let cases = [
            "sqlite::memory:",
            "sqlite://:memory:",
            "sqlite://named-managed?mode=memory&cache=private",
            "sqlite://named-managed?mode=memory&cache=shared",
        ];
        for url in cases {
            let store = ManagedStore::open(url)
                .await
                .unwrap_or_else(|error| panic!("{url}: {error}"));
            sqlx::query("INSERT INTO current_salaries (gsis_game_id, gsis_player_id, team_abbr, dfs_position, salary, provenance, import_id, source_row_id, created_at, updated_at) VALUES ('2025_01_BUF_NYJ', '00-1', 'BUF', 'QB', 8200, 'legacy', NULL, NULL, 'created', 'updated')")
                .execute(&store.writer)
                .await
                .unwrap_or_else(|error| panic!("{url}: {error}"));
            for _ in 0..5 {
                let salary: i64 = sqlx::query_scalar(
                    "SELECT salary FROM current_salaries WHERE gsis_player_id = '00-1'",
                )
                .fetch_one(&store.reader)
                .await
                .unwrap_or_else(|error| panic!("{url}: {error}"));
                assert_eq!(salary, 8200, "{url}");
            }
        }
    }

    #[tokio::test]
    async fn shared_memory_database_with_unrelated_table_is_rejected() {
        let url = "sqlite://file:guard-check?mode=memory&cache=shared";
        let db = SqlitePoolOptions::new()
            .max_connections(1)
            .connect(url)
            .await
            .unwrap();
        sqlx::query("CREATE TABLE keep_me (value TEXT NOT NULL)")
            .execute(&db)
            .await
            .unwrap();
        sqlx::query("INSERT INTO keep_me VALUES ('preserve')")
            .execute(&db)
            .await
            .unwrap();

        assert!(ManagedStore::open(url).await.is_err());
        let value: String = sqlx::query_scalar("SELECT value FROM keep_me")
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(value, "preserve");
    }
}
