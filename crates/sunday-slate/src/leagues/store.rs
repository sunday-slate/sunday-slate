use crate::leagues::model::League;
use sqlx::Sqlite;
use time::PrimitiveDateTime;

pub async fn first<'e, E>(ex: E) -> Result<Option<League>, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    sqlx::query_as!(
        League,
        r#"SELECT id AS "id!: i64",
                  name AS "name!: String",
                  created_at AS "created_at!: PrimitiveDateTime",
                  updated_at AS "updated_at!: PrimitiveDateTime"
           FROM leagues
           ORDER BY id
           LIMIT 1"#,
    )
    .fetch_optional(ex)
    .await
}

pub async fn by_id<'e, E>(ex: E, id: i64) -> Result<Option<League>, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    sqlx::query_as!(
        League,
        r#"SELECT id AS "id!: i64",
                  name AS "name!: String",
                  created_at AS "created_at!: PrimitiveDateTime",
                  updated_at AS "updated_at!: PrimitiveDateTime"
           FROM leagues
           WHERE id = ?1"#,
        id,
    )
    .fetch_optional(ex)
    .await
}

/// The leagues where `user_id` has a team, lowest id first. The first entry
/// is the fallback active league in `leagues::resolve`.
pub async fn for_user<'e, E>(ex: E, user_id: i64) -> Result<Vec<League>, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    sqlx::query_as!(
        League,
        r#"SELECT l.id AS "id!: i64",
                  l.name AS "name!: String",
                  l.created_at AS "created_at!: PrimitiveDateTime",
                  l.updated_at AS "updated_at!: PrimitiveDateTime"
           FROM leagues l
           JOIN fantasy_teams ft ON ft.league_id = l.id
           WHERE ft.user_id = ?1
           ORDER BY l.id"#,
        user_id,
    )
    .fetch_all(ex)
    .await
}

pub async fn create<'e, E>(ex: E, name: &str) -> Result<League, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    sqlx::query_as!(
        League,
        r#"INSERT INTO leagues (name) VALUES (?1)
           RETURNING id AS "id!: i64",
                     name AS "name!: String",
                     created_at AS "created_at!: PrimitiveDateTime",
                     updated_at AS "updated_at!: PrimitiveDateTime""#,
        name,
    )
    .fetch_one(ex)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::SqlitePool;

    #[sqlx::test]
    async fn first_returns_the_lowest_id_row(pool: SqlitePool) {
        assert!(first(&pool).await.expect("first").is_none());
        create(&pool, "Sunday Funday").await.expect("create");
        create(&pool, "Backyard Bowl").await.expect("create");
        let league = first(&pool).await.expect("first").expect("some league");
        assert_eq!(league.name, "Sunday Funday");
    }

    #[sqlx::test]
    async fn by_id_finds_a_league_or_none(pool: SqlitePool) {
        let created = create(&pool, "Sunday Funday").await.expect("create");
        let found = by_id(&pool, created.id)
            .await
            .expect("by_id")
            .expect("some");
        assert_eq!(found.name, "Sunday Funday");
        assert!(
            by_id(&pool, created.id + 99)
                .await
                .expect("by_id")
                .is_none()
        );
    }

    #[sqlx::test]
    async fn for_user_lists_team_leagues_lowest_id_first(pool: SqlitePool) {
        let a = create(&pool, "League A").await.expect("create");
        let b = create(&pool, "League B").await.expect("create");
        create(&pool, "League C").await.expect("create");
        let user = crate::users::store::create(&pool, "u@test.local", "hash", false)
            .await
            .expect("user");
        for league in [&b, &a] {
            crate::fantasy_teams::store::create(&pool, league.id, user.id, "T", "O", false)
                .await
                .expect("team");
        }
        let leagues = for_user(&pool, user.id).await.expect("for_user");
        let names: Vec<_> = leagues.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names, vec!["League A", "League B"]);
    }
}
