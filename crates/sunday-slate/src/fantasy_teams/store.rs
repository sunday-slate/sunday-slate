use crate::fantasy_teams::model::{FantasyTeam, FantasyTeamId, LeagueTeam};
use crate::media::MediaId;
use crate::media::store::{LogoColumns, build_logo};
use sqlx::Sqlite;
use time::PrimitiveDateTime;

pub async fn has_any<'e, E>(ex: E, league_id: i64) -> Result<bool, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    sqlx::query_scalar!(
        r#"SELECT EXISTS(SELECT 1 FROM fantasy_teams WHERE league_id = ?1) AS "exists!: bool""#,
        league_id,
    )
    .fetch_one(ex)
    .await
}

pub async fn count<'e, E>(ex: E, league_id: i64) -> Result<i64, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    sqlx::query_scalar!(
        r#"SELECT COUNT(*) AS "count!: i64" FROM fantasy_teams WHERE league_id = ?1"#,
        league_id,
    )
    .fetch_one(ex)
    .await
}

/// Every fantasy team in a league, logo resolved, ordered by name.
pub async fn by_league<'e, E>(ex: E, league_id: i64) -> Result<Vec<LeagueTeam>, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    let rows = sqlx::query!(
        r#"SELECT ft.id          AS "id!: FantasyTeamId",
                  ft.name        AS "name!: String",
                  ft.owner_name  AS "owner_name!: String",
                  m.id           AS "media_id?: MediaId",
                  m.path         AS "media_path?: String",
                  m.content_type AS "media_content_type?: String",
                  m.byte_size    AS "media_byte_size?: i64",
                  m.width        AS "media_width?: i64",
                  m.height       AS "media_height?: i64",
                  m.created_at   AS "media_created_at?: PrimitiveDateTime",
                  m.updated_at   AS "media_updated_at?: PrimitiveDateTime"
           FROM fantasy_teams ft
           LEFT JOIN media m ON m.id = ft.logo_media_id
           WHERE ft.league_id = ?1
           ORDER BY ft.name COLLATE NOCASE"#,
        league_id,
    )
    .fetch_all(ex)
    .await?;

    Ok(rows
        .into_iter()
        .map(|t| LeagueTeam {
            id: t.id,
            name: t.name,
            owner_name: t.owner_name,
            logo: build_logo(LogoColumns {
                id: t.media_id,
                path: t.media_path,
                content_type: t.media_content_type,
                byte_size: t.media_byte_size,
                width: t.media_width,
                height: t.media_height,
                created_at: t.media_created_at,
                updated_at: t.media_updated_at,
            }),
        })
        .collect())
}

pub async fn create<'e, E>(
    ex: E,
    league_id: i64,
    user_id: i64,
    name: &str,
    owner_name: &str,
    is_commissioner: bool,
) -> Result<FantasyTeam, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    sqlx::query_as!(
        FantasyTeam,
        r#"INSERT INTO fantasy_teams (league_id, user_id, name, owner_name, is_commissioner)
           VALUES (?1, ?2, ?3, ?4, ?5)
           RETURNING id AS "id!: i64",
                     league_id AS "league_id!: i64",
                     user_id AS "user_id!: i64",
                     name AS "name!: String",
                     owner_name AS "owner_name!: String",
                     logo_media_id AS "logo_media_id: MediaId",
                     is_commissioner AS "is_commissioner!: bool",
                     created_at AS "created_at!: PrimitiveDateTime",
                     updated_at AS "updated_at!: PrimitiveDateTime""#,
        league_id,
        user_id,
        name,
        owner_name,
        is_commissioner,
    )
    .fetch_one(ex)
    .await
}

pub async fn update_names<'e, E>(
    ex: E,
    team_id: i64,
    name: &str,
    owner_name: &str,
) -> Result<(), sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    sqlx::query!(
        "UPDATE fantasy_teams SET name = ?2, owner_name = ?3 WHERE id = ?1",
        team_id,
        name,
        owner_name,
    )
    .execute(ex)
    .await
    .map(|_| ())
}

/// The current user's team in a league, if they have one.
pub async fn team_for_user<'e, E>(
    ex: E,
    league_id: i64,
    user_id: i64,
) -> Result<Option<FantasyTeam>, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    sqlx::query_as!(
        FantasyTeam,
        r#"SELECT id AS "id!: i64",
                  league_id AS "league_id!: i64",
                  user_id AS "user_id!: i64",
                  name AS "name!: String",
                  owner_name AS "owner_name!: String",
                  logo_media_id AS "logo_media_id: MediaId",
                  is_commissioner AS "is_commissioner!: bool",
                  created_at AS "created_at!: PrimitiveDateTime",
                  updated_at AS "updated_at!: PrimitiveDateTime"
           FROM fantasy_teams
           WHERE league_id = ?1 AND user_id = ?2"#,
        league_id,
        user_id,
    )
    .fetch_optional(ex)
    .await
}

/// True when the user owns the commissioner team in the league.
pub async fn is_commissioner<'e, E>(
    ex: E,
    league_id: i64,
    user_id: i64,
) -> Result<bool, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    sqlx::query_scalar!(
        r#"SELECT EXISTS(
               SELECT 1 FROM fantasy_teams
               WHERE league_id = ?1 AND user_id = ?2 AND is_commissioner = 1
           ) AS "exists!: bool""#,
        league_id,
        user_id,
    )
    .fetch_one(ex)
    .await
}

pub async fn logo_media_id<'e, E>(ex: E, team_id: i64) -> Result<Option<MediaId>, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    sqlx::query_scalar!(
        r#"SELECT logo_media_id AS "logo_media_id: MediaId"
           FROM fantasy_teams WHERE id = ?1"#,
        team_id,
    )
    .fetch_one(ex)
    .await
}

pub async fn set_logo_media_id<'e, E>(
    ex: E,
    team_id: i64,
    media_id: MediaId,
) -> Result<(), sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    sqlx::query!(
        "UPDATE fantasy_teams SET logo_media_id = ?2 WHERE id = ?1",
        team_id,
        media_id,
    )
    .execute(ex)
    .await
    .map(|_| ())
}

pub async fn clear_logo<'e, E>(ex: E, team_id: i64) -> Result<(), sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    sqlx::query!(
        "UPDATE fantasy_teams SET logo_media_id = NULL WHERE id = ?1",
        team_id,
    )
    .execute(ex)
    .await
    .map(|_| ())
}

/// The league a team belongs to. `None` for an unknown team id.
pub async fn league_id_of<'e, E>(ex: E, team_id: i64) -> Result<Option<i64>, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    sqlx::query_scalar!(
        r#"SELECT league_id AS "league_id!: i64" FROM fantasy_teams WHERE id = ?1"#,
        team_id,
    )
    .fetch_optional(ex)
    .await
}

/// True when the user has a team in any league.
pub async fn any_for_user<'e, E>(ex: E, user_id: i64) -> Result<bool, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    sqlx::query_scalar!(
        r#"SELECT EXISTS(SELECT 1 FROM fantasy_teams WHERE user_id = ?1) AS "exists!: bool""#,
        user_id,
    )
    .fetch_one(ex)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::leagues::store as leagues;
    use sqlx::SqlitePool;

    /// Insert a league and a user, returning (league_id, user_id).
    async fn seed_league_and_user(pool: &SqlitePool) -> (i64, i64) {
        let league = leagues::create(pool, "Sunday Funday")
            .await
            .expect("create league");
        let user_id: i64 = sqlx::query_scalar(
            "INSERT INTO users (email, password_hash) VALUES (?1, ?2) RETURNING id",
        )
        .bind("commish@test.local")
        .bind("hash")
        .fetch_one(pool)
        .await
        .expect("create user");
        (league.id, user_id)
    }

    #[sqlx::test]
    async fn has_any_false_then_true(pool: SqlitePool) {
        let (league_id, user_id) = seed_league_and_user(&pool).await;
        assert!(!has_any(&pool, league_id).await.expect("has_any"));
        create(&pool, league_id, user_id, "Champs", "Mike", true)
            .await
            .expect("create team");
        assert!(has_any(&pool, league_id).await.expect("has_any"));
    }

    /// A second user in an existing league, so one league can hold two teams.
    async fn seed_user(pool: &SqlitePool, email: &str) -> i64 {
        sqlx::query_scalar(
            "INSERT INTO users (email, password_hash) VALUES (?1, 'hash') RETURNING id",
        )
        .bind(email)
        .fetch_one(pool)
        .await
        .expect("create user")
    }

    #[sqlx::test]
    async fn by_league_orders_by_name_ignoring_case(pool: SqlitePool) {
        let (league_id, commish) = seed_league_and_user(&pool).await;
        create(&pool, league_id, commish, "Zebra Herd", "Mike", true)
            .await
            .expect("create team");
        let second = seed_user(&pool, "two@test.local").await;
        create(&pool, league_id, second, "aardvark alliance", "Dana", false)
            .await
            .expect("create team");

        let teams = by_league(&pool, league_id).await.expect("by_league");
        let names: Vec<&str> = teams.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["aardvark alliance", "Zebra Herd"]);
        assert!(teams.iter().all(|t| t.logo.is_none()));
    }

    #[sqlx::test]
    async fn by_league_excludes_other_leagues(pool: SqlitePool) {
        let (league_id, commish) = seed_league_and_user(&pool).await;
        create(&pool, league_id, commish, "Champs", "Mike", true)
            .await
            .expect("create team");
        let other = leagues::create(&pool, "Other League")
            .await
            .expect("create league");
        let outsider = seed_user(&pool, "outsider@test.local").await;
        create(&pool, other.id, outsider, "Outsiders", "Pat", true)
            .await
            .expect("create team");

        let teams = by_league(&pool, league_id).await.expect("by_league");
        assert_eq!(teams.len(), 1);
        assert_eq!(teams[0].name, "Champs");
        assert_eq!(teams[0].owner_name, "Mike");
    }

    #[sqlx::test]
    async fn by_league_resolves_the_logo_media(pool: SqlitePool) {
        let (league_id, commish) = seed_league_and_user(&pool).await;
        let team = create(&pool, league_id, commish, "Champs", "Mike", true)
            .await
            .expect("create team");
        let media_id: MediaId = sqlx::query_scalar(
            "INSERT INTO media (path, content_type, byte_size, width, height)
             VALUES ('7.png', 'image/png', 1, 256, 256) RETURNING id",
        )
        .fetch_one(&pool)
        .await
        .expect("create media");
        set_logo_media_id(&pool, team.id, media_id)
            .await
            .expect("set logo");

        let teams = by_league(&pool, league_id).await.expect("by_league");
        let logo = teams[0].logo.as_ref().expect("logo resolved");
        assert_eq!(logo.path, "7.png");
    }

    #[sqlx::test]
    async fn create_sets_commissioner_flag(pool: SqlitePool) {
        let (league_id, user_id) = seed_league_and_user(&pool).await;
        let team = create(&pool, league_id, user_id, "Champs", "Mike", true)
            .await
            .expect("create team");
        assert!(team.is_commissioner);
        assert_eq!(team.owner_name, "Mike");
        assert!(team.logo_media_id.is_none());
    }

    #[sqlx::test]
    async fn update_names_writes_both_fields(pool: SqlitePool) {
        let (league_id, user_id) = seed_league_and_user(&pool).await;
        let team = create(&pool, league_id, user_id, "Champs", "Mike", true)
            .await
            .expect("create team");

        update_names(&pool, team.id, "Gridiron Giants", "Marcus")
            .await
            .expect("update names");

        let (name, owner_name): (String, String) =
            sqlx::query_as("SELECT name, owner_name FROM fantasy_teams WHERE id = ?1")
                .bind(team.id)
                .fetch_one(&pool)
                .await
                .expect("read names");
        assert_eq!(name, "Gridiron Giants");
        assert_eq!(owner_name, "Marcus");
    }

    #[sqlx::test]
    async fn unique_league_user_rejects_second_team(pool: SqlitePool) {
        let (league_id, user_id) = seed_league_and_user(&pool).await;
        create(&pool, league_id, user_id, "Champs", "Mike", true)
            .await
            .expect("first team");
        let second = create(&pool, league_id, user_id, "Other", "Mike", false).await;
        assert!(
            second.is_err(),
            "UNIQUE(league_id, user_id) must reject a second team"
        );
    }

    #[sqlx::test]
    async fn team_for_user_returns_none_when_no_team(pool: SqlitePool) {
        let (league_id, user_id) = seed_league_and_user(&pool).await;
        let result = team_for_user(&pool, league_id, user_id)
            .await
            .expect("team_for_user");
        assert!(result.is_none(), "no team should return None");
    }

    #[sqlx::test]
    async fn team_for_user_returns_team_when_present(pool: SqlitePool) {
        let (league_id, user_id) = seed_league_and_user(&pool).await;
        create(&pool, league_id, user_id, "Champs", "Mike", true)
            .await
            .expect("create team");
        let team = team_for_user(&pool, league_id, user_id)
            .await
            .expect("team_for_user")
            .expect("team should exist");
        assert_eq!(team.name, "Champs");
        assert!(team.is_commissioner);
    }

    #[sqlx::test]
    async fn team_for_user_none_for_wrong_user(pool: SqlitePool) {
        let (league_id, commish_id) = seed_league_and_user(&pool).await;
        let user2: i64 = sqlx::query_scalar(
            "INSERT INTO users (email, password_hash) VALUES (?1, ?2) RETURNING id",
        )
        .bind("two@test.local")
        .bind("hash")
        .fetch_one(&pool)
        .await
        .expect("user2");
        create(&pool, league_id, commish_id, "Champs", "Mike", true)
            .await
            .expect("team");
        let result = team_for_user(&pool, league_id, user2)
            .await
            .expect("team_for_user");
        assert!(result.is_none(), "user2 has no team, should be None");
    }

    #[sqlx::test]
    async fn is_commissioner_true_for_commissioner(pool: SqlitePool) {
        let (league_id, user_id) = seed_league_and_user(&pool).await;
        create(&pool, league_id, user_id, "Champs", "Mike", true)
            .await
            .expect("create team");
        assert!(
            is_commissioner(&pool, league_id, user_id)
                .await
                .expect("is_commissioner")
        );
    }

    #[sqlx::test]
    async fn is_commissioner_false_for_non_commissioner(pool: SqlitePool) {
        let (league_id, user_id) = seed_league_and_user(&pool).await;
        create(&pool, league_id, user_id, "Champs", "Mike", false)
            .await
            .expect("create team");
        assert!(
            !is_commissioner(&pool, league_id, user_id)
                .await
                .expect("is_commissioner")
        );
    }

    #[sqlx::test]
    async fn is_commissioner_false_when_no_team(pool: SqlitePool) {
        let (league_id, user_id) = seed_league_and_user(&pool).await;
        assert!(
            !is_commissioner(&pool, league_id, user_id)
                .await
                .expect("is_commissioner")
        );
    }

    #[sqlx::test]
    async fn set_and_clear_logo_media_id(pool: SqlitePool) {
        let (league_id, user_id) = seed_league_and_user(&pool).await;
        let team = create(&pool, league_id, user_id, "Champs", "Mike", true)
            .await
            .expect("create team");
        let mut conn = pool.acquire().await.expect("conn");
        let media_id = crate::media::store::insert(&mut *conn, "7.png", "image/png", 1, 256, 256)
            .await
            .expect("insert media");
        set_logo_media_id(&mut *conn, team.id, media_id)
            .await
            .expect("set logo");
        let got = team_for_user(&pool, league_id, user_id)
            .await
            .expect("team")
            .expect("some");
        assert_eq!(got.logo_media_id, Some(media_id));
        clear_logo(&mut *conn, team.id).await.expect("clear");
        let got = team_for_user(&pool, league_id, user_id)
            .await
            .expect("team")
            .expect("some");
        assert!(got.logo_media_id.is_none());
    }

    #[sqlx::test]
    async fn league_id_of_returns_the_teams_league(pool: SqlitePool) {
        let (league, user) = seed_league_and_user(&pool).await;
        let team = create(&pool, league, user, "T", "O", false)
            .await
            .expect("team");
        assert_eq!(
            league_id_of(&pool, team.id).await.expect("lookup"),
            Some(league)
        );
        assert_eq!(
            league_id_of(&pool, team.id + 99).await.expect("lookup"),
            None
        );
    }

    #[sqlx::test]
    async fn any_for_user_is_true_with_a_team_in_any_league(pool: SqlitePool) {
        let (league, user) = seed_league_and_user(&pool).await;
        assert!(!any_for_user(&pool, user).await.expect("any"));
        create(&pool, league, user, "T", "O", false)
            .await
            .expect("team");
        assert!(any_for_user(&pool, user).await.expect("any"));
    }
}
