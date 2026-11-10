use std::str::FromStr;
use std::time::Duration;

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions};
use tempfile::TempDir;

use crate::{Game, NflData, NflDataConfig};

/// A temporary `NflData` over fresh file databases with an independent
/// connection to its nflverse cache for schedule replacement.
pub(crate) struct Fixture {
    pub(crate) nfl: NflData,
    nflverse: SqlitePool,
    _dir: TempDir,
}

impl Fixture {
    pub(crate) async fn connect() -> Fixture {
        let dir = tempfile::tempdir().expect("temp dir");
        let url = |name: &str| format!("sqlite:{}", dir.path().join(name).display());
        let nfl = NflData::connect(NflDataConfig {
            nflverse_database_url: url("nflverse.db"),
            fanduel_database_url: url("fanduel.db"),
            database_url: url("store.db"),
            refresh_interval: None,
            season: crate::Season(2025),
            ..NflDataConfig::default()
        })
        .await
        .expect("nfl data");
        let nflverse_url = url("nflverse.db");
        let nflverse = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::from_str(&nflverse_url)
                    .expect("valid nflverse url")
                    .journal_mode(SqliteJournalMode::Wal)
                    .busy_timeout(Duration::from_secs(10)),
            )
            .await
            .expect("independent nflverse connection");
        Fixture {
            nfl,
            nflverse,
            // Dropped last: the directory outlives the database connections.
            _dir: dir,
        }
    }

    /// Replaces the nflverse cache's game rows with `games`, transactionally,
    /// without touching `storage/` databases.
    pub(crate) async fn replace_games(&self, games: &[Game]) {
        let mut tx = self.nflverse.begin().await.expect("nflverse transaction");
        sqlx::query("DELETE FROM games")
            .execute(&mut *tx)
            .await
            .expect("delete nflverse games");
        for game in games {
            sqlx::query(
                "INSERT INTO games \
                 (game_id, season, week, season_type, kickoff, \
                 home_team, away_team, home_score, away_score) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&game.game_id)
            .bind(game.season_week.season)
            .bind(game.season_week.week)
            .bind(if game.season_week.postseason() {
                crate::SeasonType::Post
            } else {
                crate::SeasonType::Reg
            })
            .bind(game.kickoff)
            .bind(game.home_team)
            .bind(game.away_team)
            .bind(game.home_score)
            .bind(game.away_score)
            .execute(&mut *tx)
            .await
            .expect("insert nflverse game");
        }
        tx.commit().await.expect("commit nflverse replacement");
    }
}
