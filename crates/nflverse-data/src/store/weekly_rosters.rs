use sqlx::{SqliteConnection, SqlitePool};

use crate::error::NflDataError;
use crate::model::{Season, TeamAbbr, Week, WeeklyRosterEntry};
use crate::store::Store;
use crate::store::sync_state::{self, SyncedAsset};

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
            e.season,
            e.week,
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
            e.season, season,
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
                    e.season,
                    e.week,
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
    season: Season,
    week: Week,
    team: &TeamAbbr,
) -> Result<Vec<WeeklyRosterEntry>, NflDataError> {
    Ok(sqlx::query_as!(
        WeeklyRosterEntry,
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
        season,
        week,
        team
    )
    .fetch_all(pool)
    .await?)
}

/// The most recent roster entry for one player, or None. Used to name a player
/// the `players` table does not carry — 144 of 2025's 3,133 rostered players
/// are absent from it.
pub(crate) async fn by_gsis(
    pool: &SqlitePool,
    gsis_id: &str,
) -> Result<Option<WeeklyRosterEntry>, NflDataError> {
    Ok(sqlx::query_as!(
        WeeklyRosterEntry,
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
    .await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, gsis: Option<&str>, week: u8, team: &str) -> WeeklyRosterEntry {
        WeeklyRosterEntry {
            season: Season(2025),
            week: Week(week),
            team: TeamAbbr(team.into()),
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

    fn asset(season: u16) -> SyncedAsset<'static> {
        // Leaked so the test can hold a 'static asset name per season.
        let name: &'static str = Box::leak(format!("roster_weekly_{season}.csv").into_boxed_str());
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
            &asset(2025),
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
            Season(2025),
            Week(13),
            &TeamAbbr("MIN".into()),
        )
        .await
        .unwrap();
        assert_eq!(min_13.len(), 1);
        assert_eq!(min_13[0].team, TeamAbbr("MIN".into()));

        let pit_13 = for_team_week(
            store.reader(),
            Season(2025),
            Week(13),
            &TeamAbbr("PIT".into()),
        )
        .await
        .unwrap();
        assert!(pit_13.is_empty(), "not on PIT yet in week 13");

        let pit_14 = for_team_week(
            store.reader(),
            Season(2025),
            Week(14),
            &TeamAbbr("PIT".into()),
        )
        .await
        .unwrap();
        assert_eq!(pit_14.len(), 1);
    }

    #[tokio::test]
    async fn replace_scopes_deletes_to_the_season() {
        let (_dir, store) = store().await;
        let mut e2024 = entry("Old Guy", Some("00-0000001"), 1, "GB");
        e2024.season = Season(2024);
        replace(&store, &asset(2024), Season(2024), &[e2024])
            .await
            .unwrap();
        replace(
            &store,
            &asset(2025),
            Season(2025),
            &[entry("New Guy", Some("00-0000002"), 1, "GB")],
        )
        .await
        .unwrap();

        // Re-replacing 2025 must leave 2024 alone.
        replace(
            &store,
            &asset(2025),
            Season(2025),
            &[entry("New Guy", Some("00-0000002"), 1, "GB")],
        )
        .await
        .unwrap();

        let gb_2024 = for_team_week(
            store.reader(),
            Season(2024),
            Week(1),
            &TeamAbbr("GB".into()),
        )
        .await
        .unwrap();
        assert_eq!(gb_2024.len(), 1, "2024 survived the 2025 replace");
        let gb_2025 = for_team_week(
            store.reader(),
            Season(2025),
            Week(1),
            &TeamAbbr("GB".into()),
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
            &asset(2025),
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
            Season(2025),
            Week(5),
            &TeamAbbr("GB".into()),
        )
        .await
        .unwrap();
        assert_eq!(gb.len(), 2, "both unassigned players survived");
    }
}
