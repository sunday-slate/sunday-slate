use sqlx::SqlitePool;

use crate::error::NflDataError;
use crate::model::{Season, SeasonType, SeasonWeek, TeamAbbr, TeamWeekStats, Week};
use crate::store::Store;
use crate::store::sync_state::{self, SyncedAsset};

pub(crate) async fn replace(
    store: &Store,
    asset: &SyncedAsset<'_>,
    season: Season,
    stats: &[TeamWeekStats],
) -> Result<u64, NflDataError> {
    for s in stats {
        debug_assert_eq!(
            s.season_week.season, season,
            "team week stats entry season must match the replace() season param"
        );
    }

    store
        .write_tx(async |conn| {
            sqlx::query!("DELETE FROM team_week_stats WHERE season = ?", season)
                .execute(&mut *conn)
                .await?;
            for s in stats {
                sqlx::query!(
                    r#"INSERT INTO team_week_stats
                       (season, week, season_type, team, opponent, gsis_game_id,
                        sacks, interceptions, fumble_recoveries, safeties,
                        touchdowns, blocked_kicks, conversion_returns, points_allowed)
                       VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
                    s.season_week.season,
                    s.season_week.week,
                    s.season_week.season_type,
                    s.team,
                    s.opponent,
                    s.gsis_game_id,
                    s.sacks,
                    s.interceptions,
                    s.fumble_recoveries,
                    s.safeties,
                    s.touchdowns,
                    s.blocked_kicks,
                    s.conversion_returns,
                    s.points_allowed,
                )
                .execute(&mut *conn)
                .await?;
            }
            sync_state::record(conn, asset).await?;
            Ok(stats.len() as u64)
        })
        .await
}

/// One row per team for the week, read straight from the pbp-derived table —
/// opponent and points_allowed were computed at ingest, so no joins.
pub(crate) async fn for_week(
    pool: &SqlitePool,
    season: Season,
    week: Week,
) -> Result<Vec<TeamWeekStats>, NflDataError> {
    let rows = sqlx::query!(
        r#"SELECT
               season AS "season: Season",
               week AS "week: Week",
               season_type AS "season_type: SeasonType",
               team AS "team: TeamAbbr",
               opponent AS "opponent: TeamAbbr",
               gsis_game_id,
               sacks AS "sacks: u32",
               interceptions AS "interceptions: u32",
               fumble_recoveries AS "fumble_recoveries: u32",
               safeties AS "safeties: u32",
               touchdowns AS "touchdowns: u32",
               blocked_kicks AS "blocked_kicks: u32",
               conversion_returns AS "conversion_returns: u32",
               points_allowed AS "points_allowed: i32"
           FROM team_week_stats
           WHERE season = ? AND week = ?
           ORDER BY team, season_type"#,
        season,
        week
    )
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|row| TeamWeekStats {
            season_week: SeasonWeek {
                season: row.season,
                week: row.week,
                season_type: row.season_type,
            },
            team: row.team,
            opponent: row.opponent,
            gsis_game_id: row.gsis_game_id,
            sacks: row.sacks,
            interceptions: row.interceptions,
            fumble_recoveries: row.fumble_recoveries,
            safeties: row.safeties,
            touchdowns: row.touchdowns,
            blocked_kicks: row.blocked_kicks,
            conversion_returns: row.conversion_returns,
            points_allowed: row.points_allowed,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::sync_state::SyncedAsset;

    fn row(team: &str, opp: &str) -> TeamWeekStats {
        // Distinct primes so a positional INSERT swap fails the roundtrip.
        TeamWeekStats {
            season_week: crate::model::SeasonWeek {
                season: Season(2025),
                week: Week(1),
                season_type: SeasonType::Reg,
            },
            team: team.parse().unwrap(),
            opponent: opp.parse().unwrap(),
            gsis_game_id: "2025_01_AAA_BBB".into(),
            sacks: 3,
            interceptions: 5,
            fumble_recoveries: 7,
            safeties: 11,
            touchdowns: 13,
            blocked_kicks: 17,
            conversion_returns: 19,
            points_allowed: 23,
        }
    }

    #[tokio::test]
    async fn replace_then_for_week_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&format!("sqlite://{}/t.db", dir.path().display()))
            .await
            .unwrap();
        let asset = SyncedAsset {
            release_tag: "pbp",
            asset_name: "play_by_play_2025.csv",
            updated_at: "2026-06-30T10:36:16Z",
        };
        let aaa = row("KC", "BUF");
        replace(&store, &asset, Season(2025), std::slice::from_ref(&aaa))
            .await
            .unwrap();
        let back = for_week(store.reader(), Season(2025), Week(1))
            .await
            .unwrap();
        assert_eq!(back, vec![aaa]);
    }
}
