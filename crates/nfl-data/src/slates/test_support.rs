//! Deterministic testbed for the slate facade: a temporary `NflData` whose
//! three databases live under one temporary directory, plus an independent
//! connection to its provider cache for transactional schedule replacement.

use std::str::FromStr;
use std::time::Duration;

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions};
use tempfile::TempDir;

use crate::{Game, NflData, NflDataConfig};

/// A slate facade connected to three fresh file databases under one temporary
/// directory, refresh disabled, with an independent connection to the
/// temporary provider cache for schedule replacement.
pub(crate) struct Fixture {
    pub(crate) nfl: NflData,
    provider: SqlitePool,
    /// Holds the three database files on disk for the fixture's lifetime.
    _dir: TempDir,
}

impl Fixture {
    pub(crate) async fn connect() -> Fixture {
        let dir = tempfile::tempdir().expect("temp dir");
        let url = |name: &str| format!("sqlite:{}", dir.path().join(name).display());
        let nfl = NflData::connect(NflDataConfig {
            nflverse_database_url: url("provider.db"),
            fanduel_database_url: url("fanduel.db"),
            database_url: url("store.db"),
            refresh_interval: None,
            ..NflDataConfig::default()
        })
        .await
        .expect("nfl data");
        let provider_url = url("provider.db");
        let provider = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::from_str(&provider_url)
                    .expect("valid provider url")
                    .journal_mode(SqliteJournalMode::Wal)
                    .busy_timeout(Duration::from_secs(10)),
            )
            .await
            .expect("independent provider connection");
        Fixture {
            nfl,
            provider,
            // Dropped last: the directory outlives the database connections.
            _dir: dir,
        }
    }

    /// Replace the provider cache's game rows with `games`, transactionally:
    /// schedule changes rewrite kickoffs and membership inputs without
    /// duplicating game ids and without touching `storage/` databases.
    pub(crate) async fn replace_games(&self, games: &[Game]) {
        let mut tx = self.provider.begin().await.expect("provider transaction");
        sqlx::query("DELETE FROM games")
            .execute(&mut *tx)
            .await
            .expect("delete provider games");
        for game in games {
            sqlx::query(
                "INSERT INTO games \
                 (gsis_game_id, season, week, season_type, kickoff, \
                 home_team, away_team, home_score, away_score) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&game.gsis_game_id)
            .bind(game.season)
            .bind(game.week)
            .bind(game.season_type)
            .bind(game.kickoff)
            .bind(game.home_team)
            .bind(game.away_team)
            .bind(game.home_score)
            .bind(game.away_score)
            .execute(&mut *tx)
            .await
            .expect("insert provider game");
        }
        tx.commit().await.expect("commit provider replacement");
    }
}
