use std::collections::HashMap;

use itertools::Itertools;

use crate::contests::{Contest, ContestId};
use crate::entries::model::{
    ContestEntry, EntryDetails, EntryId, EntrySlot, EntryTeam, FantasyTeamEntries,
    FantasyTeamEntry, Lineup, LineupError, NflPlayerId, RosterSlot, SlotValue,
};
use crate::fantasy_teams::FantasyTeamId;
use crate::media::MediaId;
use crate::media::store::{LogoColumns, build_logo};
use nfl_data::TeamAbbr as NflTeamAbbr;
use sqlx::{Sqlite, SqliteConnection};
use time::PrimitiveDateTime;

#[derive(Debug, thiserror::Error)]
pub enum EntriesError {
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),

    /// An entry whose slot rows don't form a complete lineup — the entries
    /// table holds complete lineups; anything less is corruption.
    #[error("entry in contest {contest:?}: {source}")]
    Lineup {
        contest: ContestId,
        source: LineupError,
    },
}

/// Every fantasy team in a league with its contest entries, lineups parsed
/// and total. Entry-less fantasy teams appear with empty `entries`.
pub async fn by_fantasy_team<'e, E>(
    ex: E,
    league_id: i64,
) -> Result<Vec<FantasyTeamEntries>, EntriesError>
where
    E: sqlx::Executor<'e, Database = Sqlite> + Copy,
{
    let teams = sqlx::query!(
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
           ORDER BY ft.id"#,
        league_id,
    )
    .fetch_all(ex)
    .await?;

    let slot_rows = sqlx::query!(
        r#"SELECT e.fantasy_team_id AS "fantasy_team_id!: FantasyTeamId",
                  e.contest_id      AS "contest_id!: ContestId",
                  s.roster_slot     AS "roster_slot!: RosterSlot",
                  s.gsis_player_id  AS "gsis_player_id?: NflPlayerId",
                  s.team_abbr       AS "team_abbr?: NflTeamAbbr"
           FROM entry_slots s
           JOIN entries e ON e.id = s.entry_id
           JOIN fantasy_teams ft ON ft.id = e.fantasy_team_id
           WHERE ft.league_id = ?1
           ORDER BY e.fantasy_team_id, e.contest_id"#,
        league_id,
    )
    .fetch_all(ex)
    .await?;

    // ORDER BY makes each entry's slot rows adjacent (and entries land in
    // contest order per team), so chunk_by yields one chunk per entry.
    let by_entry = slot_rows
        .into_iter()
        .chunk_by(|r| (r.fantasy_team_id, r.contest_id));
    let mut entries_by_team: HashMap<FantasyTeamId, Vec<FantasyTeamEntry>> = by_entry
        .into_iter()
        .map(|((team_id, contest), rows)| {
            let slots = rows.map(|r| entry_slot(r.roster_slot, r.gsis_player_id, r.team_abbr));
            Lineup::from_slots(slots)
                .map(|lineup| (team_id, FantasyTeamEntry { contest, lineup }))
                .map_err(|source| EntriesError::Lineup { contest, source })
        })
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .into_group_map();

    Ok(teams
        .into_iter()
        .map(|t| FantasyTeamEntries {
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
            entries: entries_by_team.remove(&t.id).unwrap_or_default(),
        })
        .collect())
}

/// One fantasy team in a league with its contest entries, lineups parsed and
/// typed. `None` when the id is not in this league.
pub async fn for_fantasy_team<'e, E>(
    ex: E,
    league_id: i64,
    team_id: FantasyTeamId,
) -> Result<Option<FantasyTeamEntries>, EntriesError>
where
    E: sqlx::Executor<'e, Database = Sqlite> + Copy,
{
    let Some(t) = sqlx::query!(
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
           WHERE ft.id = ?1 AND ft.league_id = ?2"#,
        team_id,
        league_id,
    )
    .fetch_optional(ex)
    .await?
    else {
        return Ok(None);
    };

    let slot_rows = sqlx::query!(
        r#"SELECT e.contest_id     AS "contest_id!: ContestId",
                  s.roster_slot    AS "roster_slot!: RosterSlot",
                  s.gsis_player_id AS "gsis_player_id?: NflPlayerId",
                  s.team_abbr      AS "team_abbr?: NflTeamAbbr"
           FROM entry_slots s
           JOIN entries e ON e.id = s.entry_id
           WHERE e.fantasy_team_id = ?1
           ORDER BY e.contest_id"#,
        team_id,
    )
    .fetch_all(ex)
    .await?;

    // ORDER BY makes each entry's rows adjacent, so chunk_by yields one chunk
    // per entry.
    let by_entry = slot_rows.into_iter().chunk_by(|r| r.contest_id);
    let entries = by_entry
        .into_iter()
        .map(|(contest, rows)| {
            let slots = rows.map(|r| entry_slot(r.roster_slot, r.gsis_player_id, r.team_abbr));
            Lineup::from_slots(slots)
                .map(|lineup| FantasyTeamEntry { contest, lineup })
                .map_err(|source| EntriesError::Lineup { contest, source })
        })
        .collect::<Result<Vec<_>, _>>()?;

    Ok(Some(FantasyTeamEntries {
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
        entries,
    }))
}

/// Each of a fantasy team's entries: contest id → entry id, for linking
/// straight to a lineup.
pub async fn entry_ids_for_team<'e, E>(
    ex: E,
    team_id: FantasyTeamId,
) -> Result<HashMap<ContestId, EntryId>, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    let rows = sqlx::query!(
        r#"SELECT id AS "id!: EntryId", contest_id AS "contest_id!: ContestId"
           FROM entries
           WHERE fantasy_team_id = ?1"#,
        team_id,
    )
    .fetch_all(ex)
    .await?;
    Ok(rows.into_iter().map(|r| (r.contest_id, r.id)).collect())
}

/// One contest's entries within one league: each entering fantasy team with
/// its total lineup. Fantasy teams without an entry in this contest do not
/// appear. Row order is by fantasy team id; callers sort for display.
pub async fn by_contest<'e, E>(
    ex: E,
    contest_id: ContestId,
    league_id: i64,
) -> Result<Vec<ContestEntry>, EntriesError>
where
    E: sqlx::Executor<'e, Database = Sqlite> + Copy,
{
    let teams = sqlx::query!(
        r#"SELECT e.id           AS "entry_id!: EntryId",
                  ft.id          AS "id!: FantasyTeamId",
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
           FROM entries e
           JOIN fantasy_teams ft ON ft.id = e.fantasy_team_id
           LEFT JOIN media m ON m.id = ft.logo_media_id
           WHERE e.contest_id = ?1 AND ft.league_id = ?2
           ORDER BY e.created_at, e.id"#,
        contest_id,
        league_id,
    )
    .fetch_all(ex)
    .await?;

    let slot_rows = sqlx::query!(
        r#"SELECT e.fantasy_team_id AS "fantasy_team_id!: FantasyTeamId",
                  s.roster_slot     AS "roster_slot!: RosterSlot",
                  s.gsis_player_id  AS "gsis_player_id?: NflPlayerId",
                  s.team_abbr       AS "team_abbr?: NflTeamAbbr"
           FROM entry_slots s
           JOIN entries e ON e.id = s.entry_id
           JOIN fantasy_teams ft ON ft.id = e.fantasy_team_id
           WHERE e.contest_id = ?1 AND ft.league_id = ?2
           ORDER BY e.fantasy_team_id"#,
        contest_id,
        league_id,
    )
    .fetch_all(ex)
    .await?;

    let mut slots_by_team: HashMap<FantasyTeamId, Vec<EntrySlot>> = slot_rows
        .into_iter()
        .map(|r| {
            (
                r.fantasy_team_id,
                entry_slot(r.roster_slot, r.gsis_player_id, r.team_abbr),
            )
        })
        .into_group_map();

    teams
        .into_iter()
        .map(|t| {
            // No slot rows means a slotless entry; `from_slots` names the
            // first missing slot.
            let slots = slots_by_team.remove(&t.id).unwrap_or_default();
            let lineup = Lineup::from_slots(slots).map_err(|source| EntriesError::Lineup {
                contest: contest_id,
                source,
            })?;
            Ok(ContestEntry {
                id: t.entry_id,
                fantasy_team: t.id,
                team_name: t.name,
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
                lineup,
            })
        })
        .collect()
}

/// Type one slot row's payload. The expects encode `entry_slots`' CHECK
/// constraint (DEF carries exactly a team abbr, player slots exactly a
/// gsis id), which the schema enforces on write.
fn entry_slot(slot: RosterSlot, gsis: Option<NflPlayerId>, team: Option<NflTeamAbbr>) -> EntrySlot {
    let value = match slot {
        RosterSlot::Def => {
            SlotValue::Defense(team.expect("entry_slots CHECK: DEF carries a team_abbr"))
        }
        _ => SlotValue::Player(
            gsis.expect("entry_slots CHECK: a player slot carries a gsis_player_id"),
        ),
    };
    EntrySlot { slot, value }
}

/// One entry with the context its page needs: lineup, contest, league, and
/// owning fantasy team. `None` for an unknown id.
pub async fn by_id<'e, E>(ex: E, id: EntryId) -> Result<Option<EntryDetails>, EntriesError>
where
    E: sqlx::Executor<'e, Database = Sqlite> + Copy,
{
    let Some(h) = sqlx::query!(
        r#"SELECT e.id            AS "id!: EntryId",
                  ft.league_id    AS "league_id!: i64",
                  c.id            AS "contest_id!: ContestId",
                  c.name          AS "contest_name!: String",
                  ft.id           AS "team_id!: FantasyTeamId",
                  ft.user_id      AS "user_id!: i64",
                  ft.name         AS "team_name!: String",
                  ft.owner_name   AS "owner_name!: String",
                  m.id            AS "media_id?: MediaId",
                  m.path          AS "media_path?: String",
                  m.content_type  AS "media_content_type?: String",
                  m.byte_size     AS "media_byte_size?: i64",
                  m.width         AS "media_width?: i64",
                  m.height        AS "media_height?: i64",
                  m.created_at    AS "media_created_at?: PrimitiveDateTime",
                  m.updated_at    AS "media_updated_at?: PrimitiveDateTime"
           FROM entries e
           JOIN contests c ON c.id = e.contest_id
           JOIN fantasy_teams ft ON ft.id = e.fantasy_team_id
           LEFT JOIN media m ON m.id = ft.logo_media_id
           WHERE e.id = ?1"#,
        id,
    )
    .fetch_optional(ex)
    .await?
    else {
        return Ok(None);
    };

    let slots = sqlx::query!(
        r#"SELECT roster_slot    AS "roster_slot!: RosterSlot",
                  gsis_player_id AS "gsis_player_id?: NflPlayerId",
                  team_abbr      AS "team_abbr?: NflTeamAbbr"
           FROM entry_slots WHERE entry_id = ?1"#,
        id,
    )
    .fetch_all(ex)
    .await?;

    let lineup = Lineup::from_slots(
        slots
            .into_iter()
            .map(|r| entry_slot(r.roster_slot, r.gsis_player_id, r.team_abbr)),
    )
    .map_err(|source| EntriesError::Lineup {
        contest: h.contest_id,
        source,
    })?;

    Ok(Some(EntryDetails {
        id: h.id,
        league_id: h.league_id,
        contest: Contest {
            id: h.contest_id,
            name: h.contest_name,
        },
        team: EntryTeam {
            id: h.team_id,
            user_id: h.user_id,
            name: h.team_name,
            owner_name: h.owner_name,
            logo: build_logo(LogoColumns {
                id: h.media_id,
                path: h.media_path,
                content_type: h.media_content_type,
                byte_size: h.media_byte_size,
                width: h.media_width,
                height: h.media_height,
                created_at: h.media_created_at,
                updated_at: h.media_updated_at,
            }),
        },
        lineup,
    }))
}

/// The league an entry belongs to, via its fantasy team. `None` for an
/// unknown entry id.
pub async fn league_id_of<'e, E>(ex: E, entry_id: i64) -> Result<Option<i64>, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    sqlx::query_scalar!(
        r#"SELECT ft.league_id AS "league_id!: i64"
           FROM entries e
           JOIN fantasy_teams ft ON ft.id = e.fantasy_team_id
           WHERE e.id = ?1"#,
        entry_id,
    )
    .fetch_optional(ex)
    .await
}

/// A contest slate's salaries: player salaries keyed by gsis id, D/ST
/// salaries keyed by team abbr. Absent picks (import gap) are not in the maps.
#[derive(Default)]
pub struct SlateSalaries {
    pub players: HashMap<NflPlayerId, i64>,
    pub defenses: HashMap<NflTeamAbbr, i64>,
}

impl SlateSalaries {
    /// File one salary row: a defense keys by team, anything else by gsis id
    /// (a player row without one is an import gap and prices nothing).
    pub fn insert_row(
        &mut self,
        is_defense: bool,
        gsis: Option<NflPlayerId>,
        team: NflTeamAbbr,
        salary: i64,
    ) {
        if is_defense {
            self.defenses.insert(team, salary);
        } else if let Some(id) = gsis {
            self.players.insert(id, salary);
        }
    }
}

/// Salaries for every game on a contest's frozen slate. One row per player or
/// team defense priced that week.
pub async fn slate_salaries<'e, E>(
    ex: E,
    contest_id: ContestId,
) -> Result<SlateSalaries, EntriesError>
where
    E: sqlx::Executor<'e, Database = Sqlite> + Copy,
{
    let rows = sqlx::query!(
        r#"SELECT s.gsis_player_id AS "gsis_player_id?: NflPlayerId",
                  s.team_abbr      AS "team_abbr!: NflTeamAbbr",
                  s.dfs_position   AS "dfs_position!: String",
                  s.salary         AS "salary!: i64"
           FROM nfl_player_salaries s
           JOIN contest_games cg ON cg.gsis_game_id = s.gsis_game_id
           WHERE cg.contest_id = ?1"#,
        contest_id,
    )
    .fetch_all(ex)
    .await?;

    let mut salaries = SlateSalaries::default();
    for r in rows {
        salaries.insert_row(
            r.dfs_position == "DST",
            r.gsis_player_id,
            r.team_abbr,
            r.salary,
        );
    }
    Ok(salaries)
}

/// Upsert the (contest, fantasy team) entry and replace its slots with the
/// lineup's nine. Runs on the caller's write connection, inside its
/// transaction.
pub async fn upsert_with_lineup(
    conn: &mut SqliteConnection,
    contest_id: ContestId,
    team_id: FantasyTeamId,
    lineup: &Lineup,
) -> Result<EntryId, sqlx::Error> {
    let id: i64 = sqlx::query_scalar(
        r#"INSERT INTO entries (contest_id, fantasy_team_id)
           VALUES (?1, ?2)
           ON CONFLICT(contest_id, fantasy_team_id)
           DO UPDATE SET updated_at = datetime('now', 'subsec')
           RETURNING id"#,
    )
    .bind(contest_id)
    .bind(team_id)
    .fetch_one(&mut *conn)
    .await?;

    sqlx::query("DELETE FROM entry_slots WHERE entry_id = ?1")
        .bind(id)
        .execute(&mut *conn)
        .await?;

    for EntrySlot { slot, value } in lineup.to_slots() {
        let slot_name = slot.to_string();
        match value {
            SlotValue::Player(player_id) => {
                sqlx::query!(
                    r#"INSERT INTO entry_slots (entry_id, roster_slot, gsis_player_id)
                       VALUES (?1, ?2, ?3)"#,
                    id,
                    slot_name,
                    player_id,
                )
                .execute(&mut *conn)
                .await?;
            }
            SlotValue::Defense(team) => {
                sqlx::query!(
                    r#"INSERT INTO entry_slots (entry_id, roster_slot, team_abbr)
                       VALUES (?1, ?2, ?3)"#,
                    id,
                    slot_name,
                    team,
                )
                .execute(&mut *conn)
                .await?;
            }
        }
    }
    Ok(EntryId(id))
}

/// The team's committed entry in a contest: its id and its lineup. The lineup
/// is `None` when the stored slots don't parse (corrupt or incomplete), which
/// still marks the entry as existing.
pub async fn lineup_for_team<'e, E>(
    ex: E,
    contest_id: ContestId,
    team_id: FantasyTeamId,
) -> Result<Option<(EntryId, Option<Lineup>)>, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Sqlite> + Copy,
{
    let rows = sqlx::query!(
        r#"SELECT e.id             AS "entry_id!: EntryId",
                  s.roster_slot    AS "roster_slot!: RosterSlot",
                  s.gsis_player_id AS "gsis_player_id?: NflPlayerId",
                  s.team_abbr      AS "team_abbr?: NflTeamAbbr"
           FROM entry_slots s
           JOIN entries e ON e.id = s.entry_id
           WHERE e.contest_id = ?1 AND e.fantasy_team_id = ?2"#,
        contest_id,
        team_id,
    )
    .fetch_all(ex)
    .await?;
    let Some(entry_id) = rows.first().map(|r| r.entry_id) else {
        return Ok(None);
    };
    let slots = rows
        .into_iter()
        .map(|r| entry_slot(r.roster_slot, r.gsis_player_id, r.team_abbr));
    Ok(Some((entry_id, Lineup::from_slots(slots).ok())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::factories::{self, TeamOptions};
    use sqlx::SqlitePool;

    #[sqlx::test]
    async fn by_fantasy_team_groups_entries_under_teams(pool: SqlitePool) {
        // A bare league (no implicit commissioner fantasy team, unlike
        // `factories::league`) so the two fantasy teams created below are the
        // league's only fantasy teams.
        let league = factories::bare_league(&pool, None).await;
        let a = factories::team(
            &pool,
            TeamOptions {
                league: Some(league.clone()),
                ..Default::default()
            },
        )
        .await;
        let b = factories::team(
            &pool,
            TeamOptions {
                league: Some(a.league.clone()),
                ..Default::default()
            },
        )
        .await;
        let c1 = factories::contest(&pool, "Week 1").await;
        let c2 = factories::contest(&pool, "Week 2").await;
        factories::full_entry(&pool, c1, a.team.id, "00-A").await;
        factories::full_entry(&pool, c2, a.team.id, "00-A").await;
        factories::full_entry(&pool, c1, b.team.id, "00-B").await;

        let teams = by_fantasy_team(&pool, a.league.id)
            .await
            .expect("aggregate");
        assert_eq!(teams.len(), 2);
        let ta = teams
            .iter()
            .find(|t| t.id == FantasyTeamId(a.team.id))
            .unwrap();
        let tb = teams
            .iter()
            .find(|t| t.id == FantasyTeamId(b.team.id))
            .unwrap();
        assert_eq!(ta.entries.len(), 2);
        assert_eq!(tb.entries.len(), 1);
        assert_eq!(tb.entries[0].contest, ContestId(c1));
        assert_eq!(tb.entries[0].lineup.rb1, NflPlayerId("00-B".into()));
        assert_eq!(ta.name, a.team.name);
        assert_eq!(ta.owner_name, a.team.owner_name);
    }

    #[sqlx::test]
    async fn by_fantasy_team_includes_entryless_teams(pool: SqlitePool) {
        // A bare league (no implicit commissioner fantasy team, unlike
        // `factories::league`) so this is the league's only fantasy team.
        let league = factories::bare_league(&pool, None).await;
        let fixture = factories::team(
            &pool,
            TeamOptions {
                league: Some(league),
                ..Default::default()
            },
        )
        .await;
        let teams = by_fantasy_team(&pool, fixture.league.id)
            .await
            .expect("aggregate");
        assert_eq!(teams.len(), 1);
        assert!(teams[0].entries.is_empty());
    }

    #[sqlx::test]
    async fn by_fantasy_team_joins_logo_media(pool: SqlitePool) {
        // A bare league (no implicit commissioner fantasy team, unlike
        // `factories::league`) so the two fantasy teams created below are the
        // league's only fantasy teams.
        let league = factories::bare_league(&pool, None).await;
        let with_logo = factories::team(
            &pool,
            TeamOptions {
                league: Some(league.clone()),
                ..Default::default()
            },
        )
        .await;
        let without_logo = factories::team(
            &pool,
            TeamOptions {
                league: Some(with_logo.league.clone()),
                ..Default::default()
            },
        )
        .await;
        let media_id: MediaId = sqlx::query_scalar(
            "INSERT INTO media (path, content_type, byte_size, width, height)
             VALUES ('7.png', 'image/png', 1, 256, 256) RETURNING id",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        sqlx::query("UPDATE fantasy_teams SET logo_media_id = ?1 WHERE id = ?2")
            .bind(media_id)
            .bind(with_logo.team.id)
            .execute(&pool)
            .await
            .unwrap();

        let teams = by_fantasy_team(&pool, with_logo.league.id)
            .await
            .expect("aggregate");
        let logoed = teams
            .iter()
            .find(|t| t.id == FantasyTeamId(with_logo.team.id))
            .unwrap();
        let bare = teams
            .iter()
            .find(|t| t.id == FantasyTeamId(without_logo.team.id))
            .unwrap();
        let logo = logoed.logo.as_ref().expect("logo media");
        assert_eq!(logo.id, media_id);
        assert_eq!(logo.path, "7.png");
        assert!(bare.logo.is_none());
    }

    #[sqlx::test]
    async fn by_fantasy_team_rejects_incomplete_lineup(pool: SqlitePool) {
        let fixture = factories::team(&pool, TeamOptions::default()).await;
        let contest = factories::contest(&pool, "Week 1").await;
        let entry_id: i64 = sqlx::query_scalar(
            "INSERT INTO entries (contest_id, fantasy_team_id) VALUES (?1, ?2) RETURNING id",
        )
        .bind(contest)
        .bind(fixture.team.id)
        .fetch_one(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO entry_slots (entry_id, roster_slot, gsis_player_id) VALUES (?1, 'RB1', '00-X')",
        )
        .bind(entry_id)
        .execute(&pool)
        .await
        .unwrap();

        let err = by_fantasy_team(&pool, fixture.league.id).await.unwrap_err();
        assert!(
            matches!(err, EntriesError::Lineup { contest: c, .. } if c == ContestId(contest)),
            "incomplete lineup is a lineup error naming the contest, got: {err:?}"
        );
    }

    #[sqlx::test]
    async fn for_fantasy_team_scopes_to_team_and_league(pool: SqlitePool) {
        let league = factories::bare_league(&pool, None).await;
        let a = factories::team(
            &pool,
            TeamOptions {
                league: Some(league.clone()),
                ..Default::default()
            },
        )
        .await;
        let b = factories::team(
            &pool,
            TeamOptions {
                league: Some(a.league.clone()),
                ..Default::default()
            },
        )
        .await;
        let c1 = factories::contest(&pool, "Week 1").await;
        factories::full_entry(&pool, c1, a.team.id, "00-A").await;
        factories::full_entry(&pool, c1, b.team.id, "00-B").await;

        let team = for_fantasy_team(&pool, a.league.id, FantasyTeamId(a.team.id))
            .await
            .expect("query")
            .expect("team in league");
        assert_eq!(team.name, a.team.name);
        assert_eq!(team.owner_name, a.team.owner_name);
        assert_eq!(team.entries.len(), 1, "only this team's entries");
        assert_eq!(team.entries[0].lineup.rb1, NflPlayerId("00-A".into()));

        let other_league = factories::bare_league(&pool, None).await;
        let cross = for_fantasy_team(&pool, other_league.id, FantasyTeamId(a.team.id))
            .await
            .expect("query");
        assert!(cross.is_none(), "wrong league resolves to None");
    }

    #[sqlx::test]
    async fn for_fantasy_team_returns_entryless_team(pool: SqlitePool) {
        let fixture = factories::team(&pool, TeamOptions::default()).await;
        let team = for_fantasy_team(&pool, fixture.league.id, FantasyTeamId(fixture.team.id))
            .await
            .expect("query")
            .expect("team present");
        assert!(team.entries.is_empty());
        assert!(team.logo.is_none());
    }

    #[sqlx::test]
    async fn by_contest_returns_one_league_and_contest_only(pool: SqlitePool) {
        // A bare league (no implicit commissioner fantasy team) so the fantasy
        // teams created below are the league's only fantasy teams.
        let league = factories::bare_league(&pool, None).await;
        let a = factories::team(
            &pool,
            TeamOptions {
                league: Some(league.clone()),
                ..Default::default()
            },
        )
        .await;
        let b = factories::team(
            &pool,
            TeamOptions {
                league: Some(a.league.clone()),
                ..Default::default()
            },
        )
        .await;
        // A fantasy team in a *different* league (TeamOptions::default creates one).
        let outsider = factories::team(&pool, TeamOptions::default()).await;

        let c1 = factories::contest(&pool, "Week 1").await;
        let c2 = factories::contest(&pool, "Week 2").await;
        let entry_a = factories::full_entry(&pool, c1, a.team.id, "00-A").await;
        factories::full_entry(&pool, c1, b.team.id, "00-B").await;
        factories::full_entry(&pool, c2, a.team.id, "00-A").await; // other contest
        factories::full_entry(&pool, c1, outsider.team.id, "00-C").await; // other league

        let rows = by_contest(&pool, ContestId(c1), a.league.id)
            .await
            .expect("load");
        assert_eq!(rows.len(), 2, "one row per entering league fantasy team");
        assert_eq!(
            rows.iter()
                .find(|r| r.id == EntryId(entry_a))
                .map(|r| r.fantasy_team),
            Some(FantasyTeamId(a.team.id)),
            "each row carries its own entry id, the /entries link target"
        );
        let ra = rows
            .iter()
            .find(|r| r.fantasy_team == FantasyTeamId(a.team.id))
            .expect("team a");
        assert_eq!(ra.team_name, a.team.name);
        assert_eq!(ra.owner_name, a.team.owner_name);
        assert_eq!(ra.lineup.rb1, NflPlayerId("00-A".into()));
        assert!(
            !rows
                .iter()
                .any(|r| r.fantasy_team == FantasyTeamId(outsider.team.id)),
            "other league's entry excluded"
        );
    }

    #[sqlx::test]
    async fn by_contest_joins_logo_media(pool: SqlitePool) {
        let fixture = factories::team(&pool, TeamOptions::default()).await;
        let contest = factories::contest(&pool, "Week 1").await;
        factories::full_entry(&pool, contest, fixture.team.id, "00-A").await;
        let media_id: MediaId = sqlx::query_scalar(
            "INSERT INTO media (path, content_type, byte_size, width, height)
             VALUES ('7.png', 'image/png', 1, 256, 256) RETURNING id",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        sqlx::query("UPDATE fantasy_teams SET logo_media_id = ?1 WHERE id = ?2")
            .bind(media_id)
            .bind(fixture.team.id)
            .execute(&pool)
            .await
            .unwrap();

        let rows = by_contest(&pool, ContestId(contest), fixture.league.id)
            .await
            .expect("load");
        let logo = rows[0].logo.as_ref().expect("logo media");
        assert_eq!(logo.path, "7.png");
    }

    #[sqlx::test]
    async fn by_contest_rejects_incomplete_lineup(pool: SqlitePool) {
        let fixture = factories::team(&pool, TeamOptions::default()).await;
        let contest = factories::contest(&pool, "Week 1").await;
        let entry_id: i64 = sqlx::query_scalar(
            "INSERT INTO entries (contest_id, fantasy_team_id) VALUES (?1, ?2) RETURNING id",
        )
        .bind(contest)
        .bind(fixture.team.id)
        .fetch_one(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO entry_slots (entry_id, roster_slot, gsis_player_id) VALUES (?1, 'RB1', '00-X')",
        )
        .bind(entry_id)
        .execute(&pool)
        .await
        .unwrap();

        let err = by_contest(&pool, ContestId(contest), fixture.league.id)
            .await
            .unwrap_err();
        assert!(
            matches!(err, EntriesError::Lineup { contest: c, .. } if c == ContestId(contest)),
            "incomplete lineup is a lineup error naming the contest, got: {err:?}"
        );
    }

    #[sqlx::test]
    async fn by_contest_empty_when_no_entries(pool: SqlitePool) {
        let fixture = factories::team(&pool, TeamOptions::default()).await;
        let contest = factories::contest(&pool, "Week 1").await;
        let rows = by_contest(&pool, ContestId(contest), fixture.league.id)
            .await
            .expect("load");
        assert!(rows.is_empty());
    }

    #[sqlx::test]
    async fn by_id_loads_entry_with_context(pool: SqlitePool) {
        let fixture = factories::team(&pool, TeamOptions::default()).await;
        let contest = factories::contest(&pool, "Week 5").await;
        let entry_id = factories::full_entry(&pool, contest, fixture.team.id, "00-A").await;

        let entry = by_id(&pool, EntryId(entry_id))
            .await
            .expect("load")
            .expect("found");
        assert_eq!(entry.id, EntryId(entry_id));
        assert_eq!(entry.league_id, fixture.league.id);
        assert_eq!(entry.contest.id, ContestId(contest));
        assert_eq!(entry.contest.name, "Week 5");
        assert_eq!(entry.team.id, FantasyTeamId(fixture.team.id));
        assert_eq!(entry.team.user_id, fixture.team.user_id);
        assert_eq!(entry.team.name, fixture.team.name);
        assert_eq!(entry.team.owner_name, fixture.team.owner_name);
        assert!(entry.team.logo.is_none());
        assert_eq!(entry.lineup.rb1, NflPlayerId("00-A".into()));
        assert_eq!(entry.lineup.def, NflTeamAbbr("KC".into()));
    }

    #[sqlx::test]
    async fn by_id_unknown_is_none(pool: SqlitePool) {
        assert!(by_id(&pool, EntryId(999)).await.expect("query").is_none());
    }

    #[sqlx::test]
    async fn slate_salaries_keys_players_and_defense(pool: SqlitePool) {
        let contest = factories::contest(&pool, "Week 1").await;
        // Freeze the slate onto one game.
        sqlx::query(
            "INSERT INTO contest_games (contest_id, gsis_game_id) VALUES (?1, '2025_01_BUF_KC')",
        )
        .bind(contest)
        .execute(&pool)
        .await
        .unwrap();
        // A player salary and a DST salary on that game.
        sqlx::query(
            "INSERT INTO nfl_player_salaries (gsis_game_id, gsis_player_id, team_abbr, dfs_position, salary)
             VALUES ('2025_01_BUF_KC', '00-A', 'KC', 'RB', 5700)",
        ).execute(&pool).await.unwrap();
        sqlx::query(
            "INSERT INTO nfl_player_salaries (gsis_game_id, gsis_player_id, team_abbr, dfs_position, salary)
             VALUES ('2025_01_BUF_KC', NULL, 'KC', 'DST', 3500)",
        ).execute(&pool).await.unwrap();
        // A salary on a game NOT on the slate must be excluded.
        sqlx::query(
            "INSERT INTO nfl_player_salaries (gsis_game_id, gsis_player_id, team_abbr, dfs_position, salary)
             VALUES ('2025_01_OTHER', '00-Z', 'SF', 'WR', 9999)",
        ).execute(&pool).await.unwrap();

        let s = slate_salaries(&pool, ContestId(contest))
            .await
            .expect("load");
        assert_eq!(s.players.get(&NflPlayerId("00-A".into())), Some(&5700));
        assert_eq!(s.defenses.get(&NflTeamAbbr("KC".into())), Some(&3500));
        assert!(
            !s.players.contains_key(&NflPlayerId("00-Z".into())),
            "off-slate salary excluded"
        );
    }

    #[sqlx::test]
    async fn league_id_of_walks_entry_to_team_to_league(pool: SqlitePool) {
        let fixture = factories::team(&pool, TeamOptions::default()).await;
        let contest = factories::contest(&pool, "Week 1").await;
        let entry_id: i64 = sqlx::query_scalar(
            "INSERT INTO entries (contest_id, fantasy_team_id) VALUES (?1, ?2) RETURNING id",
        )
        .bind(contest)
        .bind(fixture.team.id)
        .fetch_one(&pool)
        .await
        .expect("seed entry");
        assert_eq!(
            league_id_of(&pool, entry_id).await.expect("lookup"),
            Some(fixture.league.id)
        );
        assert_eq!(
            league_id_of(&pool, entry_id + 99).await.expect("lookup"),
            None
        );
    }

    #[sqlx::test]
    async fn entry_ids_for_team_maps_contest_to_entry(pool: SqlitePool) {
        let a = factories::team(&pool, TeamOptions::default()).await;
        let b = factories::team(
            &pool,
            TeamOptions {
                league: Some(a.league.clone()),
                ..Default::default()
            },
        )
        .await;
        let c1 = factories::contest(&pool, "Week 1").await;
        let c2 = factories::contest(&pool, "Week 2").await;
        let e1 = factories::full_entry(&pool, c1, a.team.id, "00-A").await;
        let e2 = factories::full_entry(&pool, c2, a.team.id, "00-A").await;
        // Another team's entry in the same contest must not leak in.
        factories::full_entry(&pool, c1, b.team.id, "00-B").await;

        let ids = entry_ids_for_team(&pool, FantasyTeamId(a.team.id))
            .await
            .expect("query");
        assert_eq!(ids.len(), 2);
        assert_eq!(ids.get(&ContestId(c1)), Some(&EntryId(e1)));
        assert_eq!(ids.get(&ContestId(c2)), Some(&EntryId(e2)));
    }
}
