use std::sync::LazyLock;

use sqlx::Sqlite;
use time::PrimitiveDateTime;

use crate::auth::password;
use crate::invites::model::{InviteToken, PendingInvite};
use crate::{AppError, Db};

/// Row returned when verifying a token: the live invite plus its league name.
#[derive(sqlx::FromRow)]
struct ActiveInviteRow {
    id: i64,
    league_id: i64,
    email: String,
    token_hash: String,
    league_name: String,
}

/// The result of a successful token verification.
#[derive(Debug, Clone)]
pub struct VerifiedInvite {
    pub invite_id: i64,
    pub league_id: i64,
    pub email: String,
    pub league_name: String,
}

/// Insert or replace the outstanding invite for `(league_id, email)`, setting a
/// 14-day expiry. Returns the invite's row id.
pub async fn upsert<'e, E>(
    ex: E,
    league_id: i64,
    email: &str,
    token_hash: &str,
) -> Result<i64, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    let email = email.to_lowercase();
    sqlx::query_scalar!(
        "INSERT INTO invites (league_id, email, token_hash, expires_at) \
         VALUES (?1, ?2, ?3, datetime('now','subsec','+14 days')) \
         ON CONFLICT (league_id, email) WHERE accepted_at IS NULL DO UPDATE SET \
             token_hash = excluded.token_hash, \
             expires_at = excluded.expires_at, \
             created_at = datetime('now','subsec') \
         RETURNING id",
        league_id,
        email,
        token_hash,
    )
    .fetch_one(ex)
    .await
}

/// Mint a new secret onto an outstanding invite: replace its hash and restart
/// the 14-day clock. Scoped to `league_id` and inert once accepted, like
/// [`revoke`]. Returns the invite's target email when a row matched — `None`
/// means the invite is gone, accepted, or belongs to another league.
///
/// Backs "regenerate link": the raw secret is unrecoverable after [`upsert`],
/// so a fresh link means a fresh secret, which retires any link sent earlier.
///
/// The expiry window is duplicated from [`upsert`] because sqlx macros need a
/// literal query string; change both together.
pub async fn reissue<'e, E>(
    ex: E,
    id: i64,
    league_id: i64,
    token_hash: &str,
) -> Result<Option<String>, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    sqlx::query_scalar!(
        r#"UPDATE invites
           SET token_hash = ?3,
               expires_at = datetime('now','subsec','+14 days')
           WHERE id = ?1 AND league_id = ?2 AND accepted_at IS NULL
           RETURNING email AS "email!: String""#,
        id,
        league_id,
        token_hash,
    )
    .fetch_optional(ex)
    .await
}

/// Mark an invite accepted, consuming it.
pub async fn mark_accepted<'e, E>(ex: E, id: i64) -> Result<(), sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    sqlx::query!(
        "UPDATE invites SET accepted_at = datetime('now','subsec') WHERE id = ?1",
        id,
    )
    .execute(ex)
    .await?;
    Ok(())
}

/// All invites for a league, newest first, for the commissioner's list.
pub async fn list_for_league<'e, E>(
    ex: E,
    league_id: i64,
) -> Result<Vec<PendingInvite>, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    sqlx::query_as!(
        PendingInvite,
        r#"SELECT id AS "id!: i64",
                  email AS "email!: String",
                  expires_at AS "expires_at!: PrimitiveDateTime",
                  accepted_at AS "accepted_at: PrimitiveDateTime"
           FROM invites
           WHERE league_id = ?1
           ORDER BY (accepted_at IS NOT NULL), created_at DESC"#,
        league_id,
    )
    .fetch_all(ex)
    .await
}

/// Delete an outstanding invite, scoped to its league. No-op if already accepted.
pub async fn revoke<'e, E>(ex: E, id: i64, league_id: i64) -> Result<(), sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    sqlx::query!(
        "DELETE FROM invites WHERE id = ?1 AND league_id = ?2 AND accepted_at IS NULL",
        id,
        league_id,
    )
    .execute(ex)
    .await?;
    Ok(())
}

/// True when `email` already owns a team in the league (already a member).
pub async fn is_member<'e, E>(ex: E, league_id: i64, email: &str) -> Result<bool, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    let email = email.to_lowercase();
    sqlx::query_scalar!(
        r#"SELECT EXISTS(
               SELECT 1 FROM fantasy_teams ft
               JOIN users u ON u.id = ft.user_id
               WHERE ft.league_id = ?1 AND u.email = ?2
           ) AS "exists!: bool""#,
        league_id,
        email,
    )
    .fetch_one(ex)
    .await
}

/// The league an invite belongs to. `None` for an unknown invite id.
pub async fn league_id_of<'e, E>(ex: E, invite_id: i64) -> Result<Option<i64>, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    sqlx::query_scalar!(
        r#"SELECT league_id AS "league_id!: i64" FROM invites WHERE id = ?1"#,
        invite_id,
    )
    .fetch_optional(ex)
    .await
}

/// An active invite for `email` in any league.
pub struct ActiveInvite {
    pub id: i64,
    pub league_id: i64,
}

/// The invite `invite_id`, when it is active (unaccepted, unexpired) and
/// addressed to `email`. Validates a session-carried pending invite.
pub async fn find_active_by_id<'e, E>(
    ex: E,
    invite_id: i64,
    email: &str,
) -> Result<Option<ActiveInvite>, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    let email = email.to_lowercase();
    sqlx::query_as!(
        ActiveInvite,
        r#"SELECT id AS "id!: i64", league_id AS "league_id!: i64"
           FROM invites
           WHERE id = ?1 AND email = ?2
             AND accepted_at IS NULL
             AND expires_at > datetime('now','subsec')"#,
        invite_id,
        email,
    )
    .fetch_optional(ex)
    .await
}

/// The most recent active (unaccepted, unexpired) invite for `email` across
/// all leagues. Resolves which league a non-admin creates their team in.
pub async fn find_active_any<'e, E>(ex: E, email: &str) -> Result<Option<ActiveInvite>, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    let email = email.to_lowercase();
    sqlx::query_as!(
        ActiveInvite,
        r#"SELECT id AS "id!: i64", league_id AS "league_id!: i64"
           FROM invites
           WHERE email = ?1
             AND accepted_at IS NULL
             AND expires_at > datetime('now','subsec')
           ORDER BY created_at DESC
           LIMIT 1"#,
        email,
    )
    .fetch_optional(ex)
    .await
}

async fn find_active<'e, E>(ex: E, id: i64) -> Result<Option<ActiveInviteRow>, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    sqlx::query_as!(
        ActiveInviteRow,
        r#"SELECT i.id AS "id!: i64",
                  i.league_id AS "league_id!: i64",
                  i.email AS "email!: String",
                  i.token_hash AS "token_hash!: String",
                  l.name AS "league_name!: String"
           FROM invites i
           JOIN leagues l ON l.id = i.league_id
           WHERE i.id = ?1
             AND i.accepted_at IS NULL
             AND i.expires_at > datetime('now','subsec')"#,
        id,
    )
    .fetch_optional(ex)
    .await
}

/// Dummy hash to keep timing uniform when no live invite matches the selector.
static DUMMY_HASH: LazyLock<String> = LazyLock::new(|| password::hash("dummy"));

/// Verify a raw `"{id}.{secret}"` token. Constant-time: always one argon2 verify.
pub async fn verify(db: &Db, raw: &str) -> Result<Option<VerifiedInvite>, AppError> {
    let parsed = raw.parse::<InviteToken>().ok();
    let row = match &parsed {
        Some(token) => find_active(db.reader(), token.id).await?,
        None => None,
    };

    let secret = parsed.as_ref().map(|t| t.secret.as_str()).unwrap_or("");
    let stored_hash = row
        .as_ref()
        .map_or(DUMMY_HASH.as_str(), |r| r.token_hash.as_str());
    if password::verify(secret, stored_hash)
        && let Some(row) = row
    {
        return Ok(Some(VerifiedInvite {
            invite_id: row.id,
            league_id: row.league_id,
            email: row.email,
            league_name: row.league_name,
        }));
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::password;
    use sqlx::SqlitePool;

    async fn seed_league(pool: &SqlitePool, name: &str) -> i64 {
        sqlx::query_scalar("INSERT INTO leagues (name) VALUES (?1) RETURNING id")
            .bind(name)
            .fetch_one(pool)
            .await
            .expect("seed league")
    }

    async fn seed_user(pool: &SqlitePool, email: &str) -> i64 {
        sqlx::query_scalar("INSERT INTO users (email, password_hash) VALUES (?1, ?2) RETURNING id")
            .bind(email)
            .bind("hash")
            .fetch_one(pool)
            .await
            .expect("seed user")
    }

    async fn seed_team(pool: &SqlitePool, league_id: i64, user_id: i64) -> i64 {
        sqlx::query_scalar(
            "INSERT INTO fantasy_teams (league_id, user_id, name, owner_name, is_commissioner) \
             VALUES (?1, ?2, ?3, ?4, ?5) RETURNING id",
        )
        .bind(league_id)
        .bind(user_id)
        .bind("Team")
        .bind("Owner")
        .bind(false)
        .fetch_one(pool)
        .await
        .expect("seed team")
    }

    #[sqlx::test]
    async fn upsert_replaces_outstanding_invite(pool: SqlitePool) {
        let league = seed_league(&pool, "L").await;
        let id1 = upsert(&pool, league, "a@b.com", "h1").await.expect("first");
        let id2 = upsert(&pool, league, "a@b.com", "h2")
            .await
            .expect("second");
        assert_eq!(id1, id2, "re-invite reuses the outstanding row");
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM invites")
            .fetch_one(&pool)
            .await
            .expect("count");
        assert_eq!(count, 1);
    }

    #[sqlx::test]
    async fn upsert_sets_a_fourteen_day_expiry(pool: SqlitePool) {
        let league = seed_league(&pool, "L").await;
        let id = upsert(&pool, league, "a@b.com", "h").await.expect("upsert");
        let days: f64 = sqlx::query_scalar(
            "SELECT julianday(expires_at) - julianday('now') FROM invites WHERE id = ?1",
        )
        .bind(id)
        .fetch_one(&pool)
        .await
        .expect("expiry");
        assert!(
            (days - 14.0).abs() < 0.01,
            "expiry is {days} days out, want 14"
        );
    }

    #[sqlx::test]
    async fn verify_roundtrips_and_rejects_bad_secret(pool: SqlitePool) {
        let league = seed_league(&pool, "Sunday Funday").await;
        let secret = "s3cr3t";
        let hash = password::hash(secret);
        let id = upsert(&pool, league, "a@b.com", &hash)
            .await
            .expect("upsert");
        let db = Db::test(pool.clone());

        let good = verify(&db, &format!("{id}.{secret}"))
            .await
            .expect("verify");
        let good = good.expect("valid");
        assert_eq!(good.email, "a@b.com");
        assert_eq!(good.league_name, "Sunday Funday");

        assert!(
            verify(&db, &format!("{id}.wrong"))
                .await
                .expect("verify")
                .is_none()
        );
        assert!(verify(&db, "not-a-token").await.expect("verify").is_none());
    }

    #[sqlx::test]
    async fn reissue_replaces_the_hash_and_restarts_the_expiry(pool: SqlitePool) {
        let league = seed_league(&pool, "L").await;
        let id = upsert(&pool, league, "a@b.com", "old-hash")
            .await
            .expect("upsert");
        sqlx::query(
            "UPDATE invites SET expires_at = datetime('now','subsec','+1 hour') WHERE id = ?1",
        )
        .bind(id)
        .execute(&pool)
        .await
        .expect("shorten expiry");

        assert_eq!(
            reissue(&pool, id, league, "new-hash")
                .await
                .expect("reissue")
                .as_deref(),
            Some("a@b.com")
        );

        let (hash, days): (String, f64) = sqlx::query_as(
            "SELECT token_hash, julianday(expires_at) - julianday('now') FROM invites WHERE id = ?1",
        )
        .bind(id)
        .fetch_one(&pool)
        .await
        .expect("row");
        assert_eq!(hash, "new-hash");
        assert!(
            (days - 14.0).abs() < 0.01,
            "expiry is {days} days out, want 14"
        );
    }

    #[sqlx::test]
    async fn reissue_refuses_an_accepted_invite(pool: SqlitePool) {
        let league = seed_league(&pool, "L").await;
        let id = upsert(&pool, league, "a@b.com", "old-hash")
            .await
            .expect("upsert");
        mark_accepted(&pool, id).await.expect("accept");

        assert!(
            reissue(&pool, id, league, "new-hash")
                .await
                .expect("reissue")
                .is_none()
        );

        let hash: String = sqlx::query_scalar("SELECT token_hash FROM invites WHERE id = ?1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .expect("row");
        assert_eq!(hash, "old-hash", "accepted invite must keep its hash");
    }

    #[sqlx::test]
    async fn reissue_refuses_an_invite_from_another_league(pool: SqlitePool) {
        let mine = seed_league(&pool, "Mine").await;
        let theirs = seed_league(&pool, "Theirs").await;
        let id = upsert(&pool, theirs, "a@b.com", "old-hash")
            .await
            .expect("upsert");

        assert!(
            reissue(&pool, id, mine, "new-hash")
                .await
                .expect("reissue")
                .is_none()
        );

        let hash: String = sqlx::query_scalar("SELECT token_hash FROM invites WHERE id = ?1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .expect("row");
        assert_eq!(
            hash, "old-hash",
            "cross-league reissue must not touch the row"
        );
    }

    #[sqlx::test]
    async fn revoke_deletes_outstanding_invite_for_league(pool: SqlitePool) {
        let league = seed_league(&pool, "L").await;
        let id = upsert(&pool, league, "a@b.com", "h").await.expect("upsert");
        revoke(&pool, id, league).await.expect("revoke");
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM invites")
            .fetch_one(&pool)
            .await
            .expect("count");
        assert_eq!(count, 0);
    }

    #[sqlx::test]
    async fn is_member_true_when_user_has_team_in_league(pool: SqlitePool) {
        let league = seed_league(&pool, "L").await;
        let user = seed_user(&pool, "member@example.com").await;
        seed_team(&pool, league, user).await;
        assert!(
            is_member(&pool, league, "member@example.com")
                .await
                .expect("is_member")
        );
        assert!(
            !is_member(&pool, league, "other@example.com")
                .await
                .expect("is_member other")
        );
    }

    #[sqlx::test]
    async fn list_for_league_returns_invites_newest_first(pool: SqlitePool) {
        let league = seed_league(&pool, "L").await;
        let id1: i64 = sqlx::query_scalar(
            "INSERT INTO invites (league_id, email, token_hash, expires_at, created_at) \
             VALUES (?1, ?2, ?3, datetime('now','+48 hours'), datetime('now','subsec','-1 hour')) \
             RETURNING id",
        )
        .bind(league)
        .bind("a@b.com")
        .bind("h1")
        .fetch_one(&pool)
        .await
        .expect("id1");
        let id2: i64 = sqlx::query_scalar(
            "INSERT INTO invites (league_id, email, token_hash, expires_at, created_at) \
             VALUES (?1, ?2, ?3, datetime('now','+48 hours'), datetime('now','subsec')) \
             RETURNING id",
        )
        .bind(league)
        .bind("c@d.com")
        .bind("h2")
        .fetch_one(&pool)
        .await
        .expect("id2");
        let list = list_for_league(&pool, league).await.expect("list");
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, id2);
        assert_eq!(list[1].id, id1);
    }

    #[sqlx::test]
    async fn list_for_league_sinks_accepted_invites_below_pending(pool: SqlitePool) {
        let league = seed_league(&pool, "L").await;
        let id1: i64 = sqlx::query_scalar(
            "INSERT INTO invites (league_id, email, token_hash, expires_at, created_at) \
             VALUES (?1, ?2, ?3, datetime('now','+48 hours'), datetime('now','subsec','-1 hour')) \
             RETURNING id",
        )
        .bind(league)
        .bind("a@b.com")
        .bind("h1")
        .fetch_one(&pool)
        .await
        .expect("id1");
        let id2: i64 = sqlx::query_scalar(
            "INSERT INTO invites (league_id, email, token_hash, expires_at, created_at) \
             VALUES (?1, ?2, ?3, datetime('now','+48 hours'), datetime('now','subsec')) \
             RETURNING id",
        )
        .bind(league)
        .bind("c@d.com")
        .bind("h2")
        .fetch_one(&pool)
        .await
        .expect("id2");

        // id2 is the newer invite, but accepting it should still drop it below
        // the older, still-pending id1.
        mark_accepted(&pool, id2).await.expect("mark_accepted");

        let list = list_for_league(&pool, league).await.expect("list");
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, id1);
        assert_eq!(list[1].id, id2);
    }

    #[sqlx::test]
    async fn expired_invite_is_not_active_or_verifiable(pool: SqlitePool) {
        let league = seed_league(&pool, "L").await;
        let secret = "s3cr3t";
        let hash = password::hash(secret);
        let id = upsert(&pool, league, "a@b.com", &hash)
            .await
            .expect("upsert");

        sqlx::query("UPDATE invites SET expires_at = datetime('now','-1 second') WHERE id = ?1")
            .bind(id)
            .execute(&pool)
            .await
            .expect("expire invite");

        assert!(
            find_active_any(&pool, "a@b.com")
                .await
                .expect("find_active_any")
                .is_none()
        );
        let db = Db::test(pool.clone());
        assert!(
            verify(&db, &format!("{id}.{secret}"))
                .await
                .expect("verify")
                .is_none()
        );
    }

    #[sqlx::test]
    async fn league_id_of_returns_the_invites_league(pool: SqlitePool) {
        let league = seed_league(&pool, "L").await;
        let id = upsert(&pool, league, "a@b.com", "hash")
            .await
            .expect("invite");
        assert_eq!(league_id_of(&pool, id).await.expect("lookup"), Some(league));
    }

    #[sqlx::test]
    async fn find_active_any_prefers_the_most_recent_and_skips_accepted(pool: SqlitePool) {
        let league_a = seed_league(&pool, "A").await;
        let league_b = seed_league(&pool, "B").await;
        assert!(
            find_active_any(&pool, "a@b.com")
                .await
                .expect("find")
                .is_none()
        );
        let first = upsert(&pool, league_a, "a@b.com", "h1")
            .await
            .expect("invite");
        // Force distinct created_at ordering (runtime SQL in tests, never macros).
        sqlx::query(
            "UPDATE invites SET created_at = datetime('now','subsec','-1 hour') WHERE id = ?1",
        )
        .bind(first)
        .execute(&pool)
        .await
        .expect("age first invite");
        let second = upsert(&pool, league_b, "A@B.com", "h2")
            .await
            .expect("invite");
        let found = find_active_any(&pool, "a@b.com")
            .await
            .expect("find")
            .expect("some");
        assert_eq!((found.id, found.league_id), (second, league_b));
        mark_accepted(&pool, second).await.expect("accept");
        let found = find_active_any(&pool, "a@b.com")
            .await
            .expect("find")
            .expect("some");
        assert_eq!(found.id, first);
    }
}
