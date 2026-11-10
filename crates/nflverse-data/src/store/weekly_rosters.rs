use sqlx::{SqliteConnection, SqlitePool};

use crate::error::NflDataError;
use crate::model::{Season, SeasonWeek, TeamAbbr, Week, WeeklyRosterEntry};
use crate::store::Store;
use crate::store::sync_state::{self, SyncedAsset};

/// One `weekly_roster_entries` row exactly as stored: the identity is
/// decomposed into its columns. The domain type carries a single
/// [`SeasonWeek`], so reads go through this adapter, which validates it.
struct Row {
    season: Season,
    week: Week,
    team: TeamAbbr,
    gsis_id: Option<String>,
    espn_id: Option<String>,
    full_name: String,
    last_name: Option<String>,
    position: Option<String>,
}

impl TryFrom<Row> for WeeklyRosterEntry {
    type Error = sqlx::Error;

    fn try_from(row: Row) -> Result<Self, Self::Error> {
        let season_week = SeasonWeek::new(row.season, row.week).ok_or_else(|| {
            sqlx::Error::Decode(format!("invalid absolute week {}", row.week.0).into())
        })?;
        Ok(Self {
            season_week,
            team: row.team,
            gsis_id: row.gsis_id,
            espn_id: row.espn_id,
            full_name: row.full_name,
            last_name: row.last_name,
            position: row.position,
        })
    }
}

/// Insert weekly roster entries directly, bypassing the sync pipeline and
/// `sync_state` bookkeeping. Runs inside a caller-supplied transaction. For
/// tests and tooling only — see `NflData::seed_weekly_roster_for_test`.
pub(crate) async fn seed(
    conn: &mut SqliteConnection,
    entries: &[WeeklyRosterEntry],
) -> Result<(), NflDataError> {
    for e in entries {
        sqlx::query!(
            r#"INSERT INTO weekly_roster_entries
               (season, week, team, gsis_id, espn_id, full_name, last_name, position)
               VALUES (?, ?, ?, ?, ?, ?, ?, ?)"#,
            e.season_week.season,
            e.season_week.week,
            e.team,
            e.gsis_id,
            e.espn_id,
            e.full_name,
            e.last_name,
            e.position
        )
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}

pub(crate) async fn replace(
    store: &Store,
    asset: &SyncedAsset<'_>,
    season: Season,
    entries: &[WeeklyRosterEntry],
) -> Result<u64, NflDataError> {
    for e in entries {
        debug_assert_eq!(
            e.season_week.season, season,
            "weekly roster entry season must match the replace() season param"
        );
    }

    store
        .write_tx(async |conn| {
            sqlx::query!("DELETE FROM weekly_roster_entries WHERE season = ?", season)
                .execute(&mut *conn)
                .await?;
            for e in entries {
                sqlx::query!(
                    r#"INSERT INTO weekly_roster_entries
                       (season, week, team, gsis_id, espn_id, full_name, last_name, position)
                       VALUES (?, ?, ?, ?, ?, ?, ?, ?)"#,
                    e.season_week.season,
                    e.season_week.week,
                    e.team,
                    e.gsis_id,
                    e.espn_id,
                    e.full_name,
                    e.last_name,
                    e.position
                )
                .execute(&mut *conn)
                .await?;
            }
            sync_state::record(conn, asset).await?;
            Ok(entries.len() as u64)
        })
        .await
}

pub(crate) async fn for_team_week(
    pool: &SqlitePool,
    season_week: SeasonWeek,
    team: &TeamAbbr,
) -> Result<Vec<WeeklyRosterEntry>, NflDataError> {
    let rows = sqlx::query_as!(
        Row,
        r#"SELECT
               season AS "season: Season",
               week AS "week: Week",
               team AS "team: TeamAbbr",
               gsis_id,
               espn_id,
               full_name,
               last_name,
               position
           FROM weekly_roster_entries
           WHERE season = ? AND week = ? AND team = ?
           ORDER BY last_name, full_name, id"#,
        season_week.season,
        season_week.week,
        team
    )
    .fetch_all(pool)
    .await?;

    rows.into_iter()
        .map(WeeklyRosterEntry::try_from)
        .collect::<Result<Vec<_>, _>>()
        .map_err(NflDataError::from)
}

/// The most recent roster entry for one player, or None. Used to name a player
/// the `players` table does not carry — 144 of 2025's 3,133 rostered players
/// are absent from it.
pub(crate) async fn by_gsis(
    pool: &SqlitePool,
    gsis_id: &str,
) -> Result<Option<WeeklyRosterEntry>, NflDataError> {
    let row = sqlx::query_as!(
        Row,
        r#"SELECT
               season AS "season: Season",
               week AS "week: Week",
               team AS "team: TeamAbbr",
               gsis_id,
               espn_id,
               full_name,
               last_name,
               position
           FROM weekly_roster_entries
           WHERE gsis_id = ?
           ORDER BY season DESC, week DESC
           LIMIT 1"#,
        gsis_id
    )
    .fetch_optional(pool)
    .await?;

    row.map(WeeklyRosterEntry::try_from)
        .transpose()
        .map_err(NflDataError::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, gsis: Option<&str>, week: u8, team: &str) -> WeeklyRosterEntry {
        WeeklyRosterEntry {
            season_week: SeasonWeek::new(Season(2025), Week(week)).unwrap(),
            team: team.parse().unwrap(),
            espn_id: None,
            gsis_id: gsis.map(String::from),
            full_name: name.into(),
            last_name: name.split_whitespace().next_back().map(String::from),
            position: Some("WR".into()),
        }
    }

    async fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&format!("sqlite://{}/t.db", dir.path().display()))
            .await
            .unwrap();
        (dir, store)
    }

    fn asset(name: &str) -> SyncedAsset<'_> {
        SyncedAsset {
            release_tag: "weekly_rosters",
            asset_name: name,
            updated_at: "2026-08-01T00:00:00Z",
        }
    }

    /// `by_gsis` has no season/week/team to narrow on, and the table's UNIQUE
    /// constraint puts `gsis_id` last, so it cannot serve the lookup. Without a
    /// dedicated index SQLite scans all ~96k rows.
    #[tokio::test]
    async fn by_gsis_lookup_uses_an_index() {
        let (_dir, store) = store().await;
        let plan: Vec<(i64, i64, i64, String)> = sqlx::query_as(
            r#"EXPLAIN QUERY PLAN
               SELECT season, week, team, gsis_id, full_name, last_name, position
               FROM weekly_roster_entries
               WHERE gsis_id = ?
               ORDER BY season DESC, week DESC
               LIMIT 1"#,
        )
        .bind("00-0030035")
        .fetch_all(store.reader())
        .await
        .unwrap();

        let detail = plan
            .iter()
            .map(|(_, _, _, d)| d.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            detail.contains("USING INDEX idx_weekly_roster_entries_gsis"),
            "expected the gsis_id index, got:\n{detail}"
        );
    }

    /// The whole point of the dataset: a traded player reads back on the team he
    /// was actually on that week, not on his final team.
    #[tokio::test]
    async fn a_traded_player_reads_back_per_week() {
        let (_dir, store) = store().await;
        replace(
            &store,
            &asset("roster_weekly_2025.csv"),
            Season(2025),
            &[
                entry("Adam Thielen", Some("00-0030035"), 13, "MIN"),
                entry("Adam Thielen", Some("00-0030035"), 14, "PIT"),
            ],
        )
        .await
        .unwrap();

        let min_13 = for_team_week(
            store.reader(),
            SeasonWeek::new(Season(2025), Week(13)).unwrap(),
            &TeamAbbr::MIN,
        )
        .await
        .unwrap();
        assert_eq!(min_13.len(), 1);
        assert_eq!(min_13[0].team, TeamAbbr::MIN);

        let pit_13 = for_team_week(
            store.reader(),
            SeasonWeek::new(Season(2025), Week(13)).unwrap(),
            &TeamAbbr::PIT,
        )
        .await
        .unwrap();
        assert!(pit_13.is_empty(), "not on PIT yet in week 13");

        let pit_14 = for_team_week(
            store.reader(),
            SeasonWeek::new(Season(2025), Week(14)).unwrap(),
            &TeamAbbr::PIT,
        )
        .await
        .unwrap();
        assert_eq!(pit_14.len(), 1);
    }

    #[tokio::test]
    async fn replace_scopes_deletes_to_the_season() {
        let (_dir, store) = store().await;
        let mut e2024 = entry("Old Guy", Some("00-0000001"), 1, "GB");
        e2024.season_week = SeasonWeek::new(Season(2024), Week(1)).unwrap();
        replace(
            &store,
            &asset("roster_weekly_2024.csv"),
            Season(2024),
            &[e2024],
        )
        .await
        .unwrap();
        replace(
            &store,
            &asset("roster_weekly_2025.csv"),
            Season(2025),
            &[entry("New Guy", Some("00-0000002"), 1, "GB")],
        )
        .await
        .unwrap();

        // Re-replacing 2025 must leave 2024 alone.
        replace(
            &store,
            &asset("roster_weekly_2025.csv"),
            Season(2025),
            &[entry("New Guy", Some("00-0000002"), 1, "GB")],
        )
        .await
        .unwrap();

        let gb_2024 = for_team_week(
            store.reader(),
            SeasonWeek::new(Season(2024), Week(1)).unwrap(),
            &TeamAbbr::GB,
        )
        .await
        .unwrap();
        assert_eq!(gb_2024.len(), 1, "2024 survived the 2025 replace");
        let gb_2025 = for_team_week(
            store.reader(),
            SeasonWeek::new(Season(2025), Week(1)).unwrap(),
            &TeamAbbr::GB,
        )
        .await
        .unwrap();
        assert_eq!(gb_2025.len(), 1);
    }

    /// Upstream lists practice-squad players with no GSIS id; several per team
    /// would collide on the UNIQUE(season, week, team, gsis_id) constraint if
    /// they were stored naively.
    #[tokio::test]
    async fn keeps_multiple_entries_without_a_gsis_id() {
        let (_dir, store) = store().await;
        replace(
            &store,
            &asset("roster_weekly_2025.csv"),
            Season(2025),
            &[
                entry("Nameless One", None, 5, "GB"),
                entry("Nameless Two", None, 5, "GB"),
            ],
        )
        .await
        .unwrap();

        let gb = for_team_week(
            store.reader(),
            SeasonWeek::new(Season(2025), Week(5)).unwrap(),
            &TeamAbbr::GB,
        )
        .await
        .unwrap();
        assert_eq!(gb.len(), 2, "both unassigned players survived");
    }

    #[tokio::test]
    async fn season_week_roundtrips_postseason_weeks_in_full() {
        let (_dir, store) = store().await;
        let mut written = Vec::new();
        for week in [18u8, 19, 22] {
            let mut roster = entry("Adam Thielen", Some("00-0030035"), week, "PIT");
            roster.espn_id = Some("16460".into());
            written.push(roster);
        }
        replace(
            &store,
            &asset("roster_weekly_2025.csv"),
            Season(2025),
            &written,
        )
        .await
        .unwrap();

        for expected in &written {
            let rows = for_team_week(store.reader(), expected.season_week, &TeamAbbr::PIT)
                .await
                .unwrap();
            assert_eq!(rows, [expected.clone()]);
            assert_eq!(
                rows[0].season_week.postseason(),
                expected.season_week.week.0 > 18
            );
        }
    }

    #[tokio::test]
    async fn season_week_scopes_reads_to_the_season() {
        let (_dir, store) = store().await;
        let mut older = entry("Old Guy", Some("00-0000001"), 1, "GB");
        older.season_week = SeasonWeek::new(Season(2024), Week(1)).unwrap();
        replace(
            &store,
            &asset("roster_weekly_2024.csv"),
            Season(2024),
            std::slice::from_ref(&older),
        )
        .await
        .unwrap();
        let newer = entry("New Guy", Some("00-0000002"), 1, "GB");
        replace(
            &store,
            &asset("roster_weekly_2025.csv"),
            Season(2025),
            std::slice::from_ref(&newer),
        )
        .await
        .unwrap();

        let after = for_team_week(store.reader(), newer.season_week, &TeamAbbr::GB)
            .await
            .unwrap();
        assert_eq!(after, [newer]);
        let before = for_team_week(store.reader(), older.season_week, &TeamAbbr::GB)
            .await
            .unwrap();
        assert_eq!(before, [older]);
    }

    #[tokio::test]
    async fn season_week_rejects_an_invalid_stored_week() {
        let (_dir, store) = store().await;
        sqlx::query(
            r#"INSERT INTO weekly_roster_entries
                   (season, week, team, gsis_id, full_name)
               VALUES (2025, 0, 'GB', '00-0000001', 'Bad Week')"#,
        )
        .execute(&store.write)
        .await
        .unwrap();

        let err = by_gsis(store.reader(), "00-0000001").await;
        assert!(
            matches!(err, Err(NflDataError::Db(sqlx::Error::Decode(_)))),
            "{err:?}"
        );
    }
}
