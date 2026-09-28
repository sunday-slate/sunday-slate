mod handlers;
pub mod model;
pub mod rule;
pub mod service;
pub mod store;

pub use model::{Contest, ContestId, NflGameId};

use crate::AppState;
use axum::Router;
use axum::routing::get;
use nfl_data::Game;
use sqlx::Sqlite;

/// Member-facing contest pages: the season list and one contest's details.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/contests", get(handlers::index))
        .route("/contests/{id}", get(handlers::show))
        .route("/contests/{id}/events", get(handlers::events))
}

/// The games the commissioner published for this contest, resolved against the
/// schedule. Empty when the slate is not published.
///
/// The publish form validates every id against the schedule before storing it,
/// and `nfl-sync` replaces a season's games in one transaction, so a stored id
/// resolves. An id that stops resolving means the schedule itself changed; the
/// answer there is to republish the contest.
pub async fn published_slate<'e, E>(
    ex: E,
    contest: ContestId,
    season_games: &[Game],
) -> Result<Vec<Game>, crate::AppError>
where
    E: sqlx::Executor<'e, Database = Sqlite> + Copy,
{
    let ids = store::game_ids(ex, contest).await?;
    Ok(resolve_games(&ids, season_games))
}

/// Resolve stored slate ids against the schedule, dropping ids it lost.
pub fn resolve_games(ids: &[String], season_games: &[Game]) -> Vec<Game> {
    ids.iter()
        .filter_map(|id| season_games.iter().find(|g| &g.gsis_game_id == id).cloned())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Db;
    use nfl_data::{Game, Season, SeasonType, TeamAbbr as NflTeamAbbr, Week};
    use sqlx::SqlitePool;
    use time::OffsetDateTime;
    use time::macros::datetime;

    fn game(id: &str, week: u8, kickoff: OffsetDateTime) -> Game {
        Game {
            gsis_game_id: id.into(),
            season: Season(2025),
            week: Week(week),
            season_type: SeasonType::Reg,
            kickoff: Some(kickoff),
            away_team: NflTeamAbbr("BUF".into()),
            home_team: NflTeamAbbr("KC".into()),
            home_score: None,
            away_score: None,
        }
    }

    #[sqlx::test]
    async fn published_slate_is_empty_when_nothing_is_published(pool: SqlitePool) {
        let db = Db::test(pool.clone());
        db.write_tx::<_, (), sqlx::Error>(async |conn| store::create(conn, "Week 5").await)
            .await
            .unwrap();
        let games = vec![game("sun", 5, datetime!(2025-10-05 17:00 UTC))];
        let slate = published_slate(&pool, ContestId(1), &games).await.unwrap();
        assert!(slate.is_empty());
    }

    #[sqlx::test]
    async fn published_slate_resolves_ids_against_the_schedule(pool: SqlitePool) {
        let db = Db::test(pool.clone());
        db.write_tx::<_, (), sqlx::Error>(async |conn| {
            store::create(conn, "Week 5").await?;
            store::set_games(conn, ContestId(1), &["sun".to_string()]).await
        })
        .await
        .unwrap();

        let games = vec![game("sun", 5, datetime!(2025-10-05 17:00 UTC))];
        let slate = published_slate(&pool, ContestId(1), &games).await.unwrap();
        assert_eq!(slate.len(), 1);
        assert_eq!(slate[0].home_team.0, "KC");
        assert_eq!(slate[0].week.0, 5);
    }

    #[sqlx::test]
    async fn published_slate_returns_only_games_the_schedule_carries(pool: SqlitePool) {
        let db = Db::test(pool.clone());
        db.write_tx::<_, (), sqlx::Error>(async |conn| {
            store::create(conn, "Week 5").await?;
            store::set_games(conn, ContestId(1), &["sun".to_string(), "gone".to_string()]).await
        })
        .await
        .unwrap();

        let games = vec![game("sun", 5, datetime!(2025-10-05 17:00 UTC))];
        let slate = published_slate(&pool, ContestId(1), &games).await.unwrap();
        assert_eq!(slate.len(), 1);
        assert_eq!(slate[0].gsis_game_id, "sun");
    }
}
