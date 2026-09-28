use crate::contests::ContestId;
use crate::draft_entry::model::{DraftEntry, DraftEntryId};
use crate::entries::{EntrySlot, Lineup, NflPlayerId};
use crate::fantasy_teams::FantasyTeamId;
use sqlx::{Sqlite, SqliteConnection};

/// Load a row by its keys, or `None`.
pub async fn by_keys<'e, E>(
    ex: E,
    contest_id: ContestId,
    fantasy_team_id: FantasyTeamId,
) -> Result<Option<DraftEntry>, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    let row = sqlx::query!(
        r#"SELECT id              AS "id!: DraftEntryId",
                  contest_id      AS "contest_id!: ContestId",
                  fantasy_team_id AS "fantasy_team_id!: FantasyTeamId",
                  qb_gsis_id      AS "qb_gsis_id?: NflPlayerId",
                  rb1_gsis_id     AS "rb1_gsis_id?: NflPlayerId",
                  rb2_gsis_id     AS "rb2_gsis_id?: NflPlayerId",
                  wr1_gsis_id     AS "wr1_gsis_id?: NflPlayerId",
                  wr2_gsis_id     AS "wr2_gsis_id?: NflPlayerId",
                  wr3_gsis_id     AS "wr3_gsis_id?: NflPlayerId",
                  te_gsis_id      AS "te_gsis_id?: NflPlayerId",
                  flex_gsis_id    AS "flex_gsis_id?: NflPlayerId",
                  def_team_abbr   AS "def_team_abbr?: nfl_data::TeamAbbr"
           FROM draft_entries
           WHERE contest_id = ?1 AND fantasy_team_id = ?2"#,
        contest_id,
        fantasy_team_id,
    )
    .fetch_optional(ex)
    .await?;

    Ok(row.map(|r| DraftEntry {
        id: r.id,
        contest_id: r.contest_id,
        fantasy_team_id: r.fantasy_team_id,
        qb: r.qb_gsis_id,
        rb1: r.rb1_gsis_id,
        rb2: r.rb2_gsis_id,
        wr1: r.wr1_gsis_id,
        wr2: r.wr2_gsis_id,
        wr3: r.wr3_gsis_id,
        te: r.te_gsis_id,
        flex: r.flex_gsis_id,
        def: r.def_team_abbr,
    }))
}

/// The resume/seed gate: the one draft row per (contest, fantasy team),
/// inserted when absent — seeded from `seed`, all-empty for `None`. Runs on
/// the caller's write connection: the read-only reader rejects INSERTs.
pub async fn get_or_create_tx(
    conn: &mut SqliteConnection,
    contest_id: ContestId,
    fantasy_team_id: FantasyTeamId,
    seed: Option<&Lineup>,
) -> Result<DraftEntry, sqlx::Error> {
    if let Some(row) = by_keys(&mut *conn, contest_id, fantasy_team_id).await? {
        return Ok(row);
    }
    insert_draft(&mut *conn, contest_id, fantasy_team_id, seed).await
}

async fn insert_draft<'e, E>(
    ex: E,
    contest_id: ContestId,
    fantasy_team_id: FantasyTeamId,
    seed: Option<&Lineup>,
) -> Result<DraftEntry, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    let mut draft = DraftEntry {
        id: DraftEntryId(0),
        contest_id,
        fantasy_team_id,
        qb: None,
        rb1: None,
        rb2: None,
        wr1: None,
        wr2: None,
        wr3: None,
        te: None,
        flex: None,
        def: None,
    };
    if let Some(lineup) = seed {
        for EntrySlot { slot, value } in lineup.to_slots() {
            draft.set(slot, value);
        }
    }
    let id: DraftEntryId = sqlx::query_scalar(
        r#"INSERT INTO draft_entries
             (contest_id, fantasy_team_id, qb_gsis_id, rb1_gsis_id, rb2_gsis_id,
              wr1_gsis_id, wr2_gsis_id, wr3_gsis_id, te_gsis_id, flex_gsis_id, def_team_abbr)
           VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
           RETURNING id"#,
    )
    .bind(contest_id)
    .bind(fantasy_team_id)
    .bind(&draft.qb)
    .bind(&draft.rb1)
    .bind(&draft.rb2)
    .bind(&draft.wr1)
    .bind(&draft.wr2)
    .bind(&draft.wr3)
    .bind(&draft.te)
    .bind(&draft.flex)
    .bind(&draft.def)
    .fetch_one(ex)
    .await?;

    draft.id = id;
    Ok(draft)
}

/// Write every pick column of an existing draft row.
pub async fn save<'e, E>(ex: E, draft: &DraftEntry) -> Result<(), sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    sqlx::query!(
        r#"UPDATE draft_entries SET
              qb_gsis_id = ?2, rb1_gsis_id = ?3, rb2_gsis_id = ?4,
              wr1_gsis_id = ?5, wr2_gsis_id = ?6, wr3_gsis_id = ?7,
              te_gsis_id = ?8, flex_gsis_id = ?9, def_team_abbr = ?10
           WHERE id = ?1"#,
        draft.id,
        draft.qb,
        draft.rb1,
        draft.rb2,
        draft.wr1,
        draft.wr2,
        draft.wr3,
        draft.te,
        draft.flex,
        draft.def,
    )
    .execute(ex)
    .await?;
    Ok(())
}

pub async fn delete<'e, E>(ex: E, draft_id: DraftEntryId) -> Result<(), sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    sqlx::query!("DELETE FROM draft_entries WHERE id = ?1", draft_id)
        .execute(ex)
        .await?;
    Ok(())
}

pub async fn delete_by_keys<'e, E>(
    ex: E,
    contest_id: ContestId,
    fantasy_team_id: FantasyTeamId,
) -> Result<(), sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    sqlx::query!(
        "DELETE FROM draft_entries WHERE contest_id = ?1 AND fantasy_team_id = ?2",
        contest_id,
        fantasy_team_id,
    )
    .execute(ex)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contests::ContestId;
    use crate::entries::NflPlayerId;
    use crate::fantasy_teams::FantasyTeamId;
    use crate::tests::factories::{self, TeamOptions};
    use nfl_data::TeamAbbr as NflTeamAbbr;
    use sqlx::SqlitePool;

    fn seed_lineup() -> Lineup {
        Lineup {
            qb: NflPlayerId("00-Q".into()),
            rb1: NflPlayerId("00-R1".into()),
            rb2: NflPlayerId("00-R2".into()),
            wr1: NflPlayerId("00-W1".into()),
            wr2: NflPlayerId("00-W2".into()),
            wr3: NflPlayerId("00-W3".into()),
            te: NflPlayerId("00-T".into()),
            flex: NflPlayerId("00-F".into()),
            def: NflTeamAbbr("KC".into()),
        }
    }

    #[sqlx::test]
    async fn get_or_create_seeds_on_first_call_and_resumes_after(pool: SqlitePool) {
        let fixture = factories::team(&pool, TeamOptions::default()).await;
        let contest = factories::contest(&pool, "Week 1").await;
        let mut conn = pool.acquire().await.expect("conn");

        // First call: no row, seeds from the lineup.
        let d = get_or_create_tx(
            &mut conn,
            ContestId(contest),
            FantasyTeamId(fixture.team.id),
            Some(&seed_lineup()),
        )
        .await
        .expect("create");
        assert_eq!(d.qb, Some(NflPlayerId("00-Q".into())));
        assert_eq!(d.def, Some(NflTeamAbbr("KC".into())));

        // Second call: returns the existing row, ignoring a new seed.
        let d2 = get_or_create_tx(
            &mut conn,
            ContestId(contest),
            FantasyTeamId(fixture.team.id),
            None,
        )
        .await
        .expect("resume");
        assert_eq!(d2.id, d.id);
        assert_eq!(d2.qb, Some(NflPlayerId("00-Q".into())));
    }

    #[sqlx::test]
    async fn duplicate_player_across_columns_violates_check(pool: SqlitePool) {
        let fixture = factories::team(&pool, TeamOptions::default()).await;
        let contest = factories::contest(&pool, "Week 1").await;
        let inserted = sqlx::query(
            "INSERT INTO draft_entries (contest_id, fantasy_team_id, qb_gsis_id, rb1_gsis_id)
             VALUES (?1, ?2, '00-X', '00-X')",
        )
        .bind(contest)
        .bind(fixture.team.id)
        .execute(&pool)
        .await;
        assert!(inserted.is_err(), "duplicate gsis across columns must fail");
    }
}
