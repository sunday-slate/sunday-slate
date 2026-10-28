//! Durable slate metadata and atomic weekly membership snapshots.

use std::collections::HashMap;

use sqlx::{Executor, Sqlite, SqliteConnection, SqlitePool};
use time::{OffsetDateTime, UtcOffset};

use crate::slates::{Slate, SlateId, SlateProposal};
use crate::store::Store;
use crate::{Game, Season, SeasonWeek};

/// Every stored slate for a season, ordered by slate id.
pub(crate) async fn for_season(
    pool: &SqlitePool,
    season: Season,
) -> Result<Vec<Slate>, sqlx::Error> {
    let Some((first, last)) = season_bounds(season) else {
        return Ok(Vec::new());
    };
    sqlx::query_as::<_, Slate>(
        r#"SELECT id, season_week, name, kickoff
               FROM slates
               WHERE season_week BETWEEN ? AND ?
               ORDER BY id"#,
    )
    .bind(first)
    .bind(last)
    .fetch_all(pool)
    .await
}

/// Games per slate for a season; a slate with no membership counts zero.
pub(crate) async fn counts_for_season(
    pool: &SqlitePool,
    season: Season,
) -> Result<HashMap<SlateId, usize>, sqlx::Error> {
    let Some((first, last)) = season_bounds(season) else {
        return Ok(HashMap::new());
    };
    let rows: Vec<(i64, i64)> = sqlx::query_as(
        r#"SELECT slates.id, COUNT(slate_games.gsis_game_id)
               FROM slates
               LEFT JOIN slate_games ON slate_games.slate_id = slates.id
               WHERE slates.season_week BETWEEN ? AND ?
               GROUP BY slates.id"#,
    )
    .bind(first)
    .bind(last)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(id, count)| (SlateId(id), count as usize))
        .collect())
}

/// The encoded week range a season spans, or `None` with no weeks.
fn season_bounds(season: Season) -> Option<(i64, i64)> {
    let mut weeks = SeasonWeek::for_season(season);
    let first = i64::from(weeks.next()?.encode());
    let last = i64::from(weeks.last()?.encode());
    Some((first, last))
}

/// Every stored slate for an NFL week, ordered by known kickoff ascending,
/// unknown kickoff last, id breaking ties.
pub(crate) async fn for_week(
    pool: &SqlitePool,
    week: SeasonWeek,
) -> Result<Vec<Slate>, sqlx::Error> {
    for_week_on(pool, week).await
}

async fn for_week_on<'e, E>(ex: E, week: SeasonWeek) -> Result<Vec<Slate>, sqlx::Error>
where
    E: Executor<'e, Database = Sqlite>,
{
    sqlx::query_as::<_, Slate>(
        r#"SELECT id, season_week, name, kickoff
               FROM slates
               WHERE season_week = ?
               ORDER BY id"#,
    )
    .bind(week)
    .fetch_all(ex)
    .await
}

pub(crate) async fn by_id(pool: &SqlitePool, id: SlateId) -> Result<Option<Slate>, sqlx::Error> {
    sqlx::query_as::<_, Slate>(
        r#"SELECT id, season_week, name, kickoff
               FROM slates
               WHERE id = ?"#,
    )
    .bind(id)
    .fetch_optional(pool)
    .await
}

/// True iff the week's slates have been fixed; membership rows exist for
/// tentative weeks too, so presence alone no longer means fixed.
pub(crate) async fn is_fixed(pool: &SqlitePool, week: SeasonWeek) -> Result<bool, sqlx::Error> {
    is_fixed_on(pool, week).await
}

async fn is_fixed_on<'e, E>(ex: E, week: SeasonWeek) -> Result<bool, sqlx::Error>
where
    E: Executor<'e, Database = Sqlite>,
{
    let fixed: i64 = sqlx::query_scalar(
        r#"SELECT EXISTS (
               SELECT 1
                   FROM slates
                   WHERE season_week = ? AND fixed = 1)"#,
    )
    .bind(week)
    .fetch_one(ex)
    .await?;
    Ok(fixed != 0)
}

/// The slate's stored membership ids in lexical order.
pub(crate) async fn game_ids(pool: &SqlitePool, id: SlateId) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar::<_, String>(
        r#"SELECT gsis_game_id
               FROM slate_games
               WHERE slate_id = ?
               ORDER BY gsis_game_id"#,
    )
    .bind(id)
    .fetch_all(pool)
    .await
}

/// Reconciles the week in one `BEGIN IMMEDIATE` transaction; a fixed week's stored snapshot wins.
pub(crate) async fn reconcile(
    store: &Store,
    week: SeasonWeek,
    proposals: &[SlateProposal],
    games: &[Game],
    fix: bool,
) -> Result<Vec<Slate>, sqlx::Error> {
    store
        .write_tx(async |conn| reconcile_tx(&mut *conn, week, proposals, games, fix).await)
        .await
}

async fn reconcile_tx(
    conn: &mut SqliteConnection,
    week: SeasonWeek,
    proposals: &[SlateProposal],
    games: &[Game],
    fix: bool,
) -> Result<Vec<Slate>, sqlx::Error> {
    if is_fixed_on(&mut *conn, week).await? {
        refresh_fixed_kickoffs(&mut *conn, week, games).await?;
    } else {
        for proposal in proposals {
            upsert_metadata(&mut *conn, week, proposal).await?;
        }
        clear_absent_kickoffs(&mut *conn, week, proposals).await?;
        clear_week_membership(&mut *conn, week).await?;
        for proposal in proposals {
            for game_id in &proposal.game_ids {
                sqlx::query("INSERT INTO slate_games (slate_id, gsis_game_id) VALUES (?, ?)")
                    .bind(proposal.id)
                    .bind(game_id)
                    .execute(&mut *conn)
                    .await?;
            }
        }
        if fix {
            sqlx::query("UPDATE slates SET fixed = 1 WHERE season_week = ?")
                .bind(week)
                .execute(&mut *conn)
                .await?;
        }
    }
    for_week_on(&mut *conn, week).await
}

/// Drops the week's tentative membership before it is rewritten, so games that
/// left a proposal leave the snapshot too.
async fn clear_week_membership(
    conn: &mut SqliteConnection,
    week: SeasonWeek,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"DELETE FROM slate_games
               WHERE slate_id IN (SELECT id FROM slates WHERE season_week = ?)"#,
    )
    .bind(week)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

async fn upsert_metadata(
    conn: &mut SqliteConnection,
    week: SeasonWeek,
    proposal: &SlateProposal,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"INSERT INTO slates (id, season_week, name, kickoff) VALUES (?, ?, ?, ?)
               ON CONFLICT (season_week, name) DO UPDATE SET kickoff = excluded.kickoff"#,
    )
    .bind(proposal.id)
    .bind(week)
    .bind(&proposal.name)
    .bind(proposal.kickoff)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Clears stored kickoffs for week rows absent from the current slate game proposal
async fn clear_absent_kickoffs(
    conn: &mut SqliteConnection,
    week: SeasonWeek,
    proposals: &[SlateProposal],
) -> Result<(), sqlx::Error> {
    let stored: Vec<String> =
        sqlx::query_scalar(r#"SELECT name FROM slates WHERE season_week = ?"#)
            .bind(week)
            .fetch_all(&mut *conn)
            .await?;
    for name in stored {
        if !proposals.iter().any(|proposal| proposal.name == name) {
            sqlx::query("UPDATE slates SET kickoff = NULL WHERE season_week = ? AND name = ?")
                .bind(week)
                .bind(&name)
                .execute(&mut *conn)
                .await?;
        }
    }
    Ok(())
}

/// Refreshes each stored kickoff from its fixed members' earliest known
/// kickoff in the supplied schedule; `None` when none is known.
async fn refresh_fixed_kickoffs(
    conn: &mut SqliteConnection,
    week: SeasonWeek,
    games: &[Game],
) -> Result<(), sqlx::Error> {
    let schedule: HashMap<&str, OffsetDateTime> = games
        .iter()
        .filter_map(|game| {
            game.kickoff.map(|kickoff| {
                (
                    game.gsis_game_id.as_str(),
                    kickoff.to_offset(UtcOffset::UTC),
                )
            })
        })
        .collect();

    let ids: Vec<SlateId> = sqlx::query_scalar(r#"SELECT id FROM slates WHERE season_week = ?"#)
        .bind(week)
        .fetch_all(&mut *conn)
        .await?;
    for id in ids {
        let members: Vec<String> =
            sqlx::query_scalar(r#"SELECT gsis_game_id FROM slate_games WHERE slate_id = ?"#)
                .bind(id)
                .fetch_all(&mut *conn)
                .await?;
        let kickoff = members
            .iter()
            .filter_map(|game_id| schedule.get(game_id.as_str()).copied())
            .min();
        sqlx::query("UPDATE slates SET kickoff = ? WHERE id = ?")
            .bind(kickoff)
            .bind(id)
            .execute(&mut *conn)
            .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use time::OffsetDateTime;
    use time::macros::datetime;

    use super::{by_id, for_week, game_ids, is_fixed, reconcile};
    use crate::slates::{SlateId, SlateProposal};
    use crate::store::Store;
    use crate::store::player_link::{links, save_links};
    use crate::{Game, PlayerLink, Season, SeasonType, SeasonWeek, Week};

    async fn store() -> Store {
        Store::in_memory().await.expect("in-memory store")
    }

    fn reg_week(week: u8) -> SeasonWeek {
        SeasonWeek {
            season: Season(2025),
            season_type: SeasonType::Reg,
            week: Week(week),
        }
    }

    fn post_week(week: u8) -> SeasonWeek {
        SeasonWeek {
            season: Season(2025),
            season_type: SeasonType::Post,
            week: Week(week),
        }
    }

    fn game(id: &str, kickoff: Option<OffsetDateTime>) -> Game {
        Game {
            gsis_game_id: id.into(),
            season: Season(2025),
            week: Week(5),
            season_type: SeasonType::Reg,
            kickoff,
            away_team: "KC".parse().unwrap(),
            home_team: "BUF".parse().unwrap(),
            home_score: None,
            away_score: None,
        }
    }

    fn proposal(
        week: SeasonWeek,
        name: &str,
        kickoff: Option<OffsetDateTime>,
        ids: &[&str],
    ) -> SlateProposal {
        SlateProposal {
            id: SlateId(i64::from(week.encode()) * 10 + kind(name)),
            name: name.into(),
            kickoff,
            game_ids: ids.iter().map(|id| id.to_string()).collect(),
        }
    }

    fn kind(name: &str) -> i64 {
        match name {
            "Thanksgiving" | "Christmas" => 0,
            _ => 1,
        }
    }

    #[tokio::test]
    async fn metadata_roundtrips_without_provider_or_games() {
        let store = store().await;
        let week = reg_week(5);

        let first = reconcile(
            &store,
            week,
            &[proposal(week, "Week 5", None, &[])],
            &[],
            false,
        )
        .await
        .expect("reconcile");
        assert_eq!(first.len(), 1, "the proposal's metadata is stored");

        let slate = by_id(store.reader(), first[0].id)
            .await
            .expect("by id")
            .expect("stored slate");
        assert_eq!(slate.id, first[0].id);
        assert_eq!(slate.season_week, week);
        assert_eq!(slate.name, "Week 5");
        assert_eq!(
            slate.kickoff, None,
            "the unknown kickoff survives the round trip"
        );
    }

    #[tokio::test]
    async fn repeated_reconciliation_preserves_assigned_ids() {
        let store = store().await;

        let week = reg_week(5);
        let proposals = [proposal(week, "Week 5", None, &[])];
        let first = reconcile(&store, week, &proposals, &[], false)
            .await
            .expect("first reconcile");
        let second = reconcile(&store, week, &proposals, &[], false)
            .await
            .expect("second reconcile");
        assert_eq!(first[0].id, second[0].id);
        assert_eq!(
            first[0].id,
            SlateId(20250051),
            "REG 2025 week 5's main slate carries its derived id"
        );

        let week_13 = reg_week(13);
        let established = reconcile(
            &store,
            week_13,
            &[
                proposal(week_13, "Week 13", None, &[]),
                proposal(week_13, "Thanksgiving", None, &[]),
            ],
            &[],
            false,
        )
        .await
        .expect("week 13 reconcile");
        assert_eq!(
            established
                .iter()
                .find(|slate| slate.name == "Week 13")
                .expect("the main slate")
                .id,
            SlateId(20250131)
        );
        assert_eq!(
            established
                .iter()
                .find(|slate| slate.name == "Thanksgiving")
                .expect("the holiday slate")
                .id,
            SlateId(20250130)
        );

        let other_week = reconcile(
            &store,
            reg_week(6),
            &[proposal(reg_week(6), "Week 6", None, &[])],
            &[],
            false,
        )
        .await
        .expect("other week reconcile");
        assert_eq!(other_week.len(), 1);
        assert_eq!(other_week[0].id, SlateId(20250061));
        assert_ne!(
            first[0].id, other_week[0].id,
            "a different week gets its own id"
        );

        let other_name = reconcile(
            &store,
            week,
            &[proposal(week, "Thanksgiving", None, &[])],
            &[],
            false,
        )
        .await
        .expect("other name reconcile");
        let thanksgiving = other_name
            .iter()
            .find(|slate| slate.name == "Thanksgiving")
            .expect("the thanksgiving slate");
        assert_eq!(thanksgiving.id, SlateId(20250050));
        assert_ne!(
            first[0].id, thanksgiving.id,
            "a different name gets its own id"
        );
    }

    #[tokio::test]
    async fn metadata_queries_are_week_scoped_and_chronological() {
        let store = store().await;

        let week = reg_week(13);
        reconcile(
            &store,
            week,
            &[
                proposal(week, "Week 13", Some(datetime!(2025-11-27 18:00 UTC)), &[]),
                proposal(
                    week,
                    "Thanksgiving",
                    Some(datetime!(2025-11-30 17:00 UTC)),
                    &[],
                ),
            ],
            &[],
            false,
        )
        .await
        .expect("reconcile");
        let got = for_week(store.reader(), week).await.expect("for week");
        let names: Vec<&str> = got.iter().map(|slate| slate.name.as_str()).collect();
        assert_eq!(
            names,
            ["Week 13", "Thanksgiving"],
            "known kickoffs ascending"
        );

        let tie_week = reg_week(12);
        reconcile(
            &store,
            tie_week,
            &[
                proposal(
                    tie_week,
                    "Sunday Night",
                    Some(datetime!(2025-11-27 18:00 UTC)),
                    &[],
                ),
                proposal(
                    tie_week,
                    "Thanksgiving",
                    Some(datetime!(2025-11-27 18:00 UTC)),
                    &[],
                ),
            ],
            &[],
            false,
        )
        .await
        .expect("tie reconcile");
        let tied = for_week(store.reader(), tie_week).await.expect("tie week");
        assert_eq!(
            tied.iter()
                .map(|slate| slate.name.as_str())
                .collect::<Vec<_>>(),
            ["Thanksgiving", "Sunday Night"],
            "the derived ids break the tie: holiday kind first"
        );
        assert_eq!(tied[0].kickoff, tied[1].kickoff, "both share a kickoff");
        assert!(tied[0].id < tied[1].id, "the id breaks the kickoff tie");

        let unknown_week = reg_week(11);
        reconcile(
            &store,
            unknown_week,
            &[
                proposal(
                    unknown_week,
                    "Week 11",
                    Some(datetime!(2025-11-27 18:00 UTC)),
                    &[],
                ),
                proposal(unknown_week, "Thanksgiving", None, &[]),
            ],
            &[],
            false,
        )
        .await
        .expect("unknown reconcile");
        let unknown = for_week(store.reader(), unknown_week)
            .await
            .expect("unknown week");
        assert_eq!(unknown[1].kickoff, None, "unknown kickoff is last");

        reconcile(
            &store,
            reg_week(18),
            &[proposal(reg_week(18), "Week 18", None, &[])],
            &[],
            false,
        )
        .await
        .expect("regular reconcile");
        reconcile(
            &store,
            post_week(19),
            &[proposal(post_week(19), "Wild Card", None, &[])],
            &[],
            false,
        )
        .await
        .expect("postseason reconcile");
        let reg = for_week(store.reader(), reg_week(18))
            .await
            .expect("reg week");
        let post = for_week(store.reader(), post_week(19))
            .await
            .expect("post week");
        assert_eq!(
            reg.iter()
                .map(|slate| slate.name.as_str())
                .collect::<Vec<_>>(),
            ["Week 18"]
        );
        assert_eq!(
            post.iter()
                .map(|slate| slate.name.as_str())
                .collect::<Vec<_>>(),
            ["Wild Card"]
        );
        assert_ne!(
            reg[0].id, post[0].id,
            "the last regular week and the first postseason week stay distinct"
        );
    }

    #[tokio::test]
    async fn metadata_without_memberships_reads_unfixed() {
        let store = store().await;
        let week = reg_week(5);

        reconcile(
            &store,
            week,
            &[proposal(week, "Week 5", None, &[])],
            &[],
            false,
        )
        .await
        .expect("tentative reconcile");
        assert!(
            !for_week(store.reader(), week)
                .await
                .expect("for week")
                .is_empty(),
            "the metadata row is stored"
        );
        assert!(
            !is_fixed(store.reader(), week).await.unwrap(),
            "no membership rows exist, so the week reads unfixed"
        );
    }

    #[tokio::test]
    async fn fixed_week_ignores_replacement_proposals() {
        let store = store().await;
        let week = reg_week(5);
        let games = [
            game("a", Some(datetime!(2025-10-05 17:00 UTC))),
            game("b", Some(datetime!(2025-10-05 20:25 UTC))),
        ];
        let initial = [proposal(
            week,
            "Week 5",
            Some(datetime!(2025-10-05 17:00 UTC)),
            &["a", "b"],
        )];

        let first = reconcile(&store, week, &initial, &games, true)
            .await
            .expect("fix");
        assert_eq!(first.len(), 1);
        let id = first[0].id;
        assert_eq!(game_ids(store.reader(), id).await.unwrap(), vec!["a", "b"]);
        assert!(is_fixed(store.reader(), week).await.unwrap());

        let replacement = [proposal(
            week,
            "Week 5",
            Some(datetime!(2025-10-05 16:00 UTC)),
            &["b", "c"],
        )];
        let schedule = [
            game("a", Some(datetime!(2025-10-05 17:00 UTC))),
            game("b", Some(datetime!(2025-10-05 20:25 UTC))),
            game("c", Some(datetime!(2025-10-05 16:00 UTC))),
        ];
        let second = reconcile(&store, week, &replacement, &schedule, false)
            .await
            .expect("replacement reconcile");
        assert_eq!(second.len(), 1, "no replacement slate was created");
        assert_eq!(second[0].id, id, "the stored slate keeps its id");
        assert_eq!(
            game_ids(store.reader(), id).await.unwrap(),
            vec!["a", "b"],
            "the replacement proposal did not change the snapshot"
        );
        assert!(is_fixed(store.reader(), week).await.unwrap());
    }

    #[tokio::test]
    async fn fixed_kickoff_tracks_members_not_new_rule_matches() {
        let store = store().await;
        let week = reg_week(5);
        let initial = [
            game("a", Some(datetime!(2025-10-05 17:00 UTC))),
            game("b", Some(datetime!(2025-10-05 20:25 UTC))),
        ];
        let proposals = [proposal(
            week,
            "Week 5",
            Some(datetime!(2025-10-05 17:00 UTC)),
            &["a", "b"],
        )];

        let first = reconcile(&store, week, &proposals, &initial, true)
            .await
            .expect("fix");
        assert_eq!(first[0].kickoff, Some(datetime!(2025-10-05 17:00 UTC)));

        let current = [
            game("a", Some(datetime!(2025-10-05 21:30 UTC))),
            game("b", Some(datetime!(2025-10-05 20:25 UTC))),
            game("c", Some(datetime!(2025-10-05 16:00 UTC))),
        ];
        let replacement = [proposal(
            week,
            "Week 5",
            Some(datetime!(2025-10-05 16:00 UTC)),
            &["b", "c"],
        )];
        let second = reconcile(&store, week, &replacement, &current, false)
            .await
            .expect("reconcile after fix");

        assert_eq!(
            second[0].kickoff,
            Some(datetime!(2025-10-05 20:25 UTC)),
            "the stored kickoff follows members a and b, not the new nonmember"
        );
        assert_eq!(
            game_ids(store.reader(), second[0].id).await.unwrap(),
            vec!["a", "b"]
        );
    }

    #[tokio::test]
    async fn omitted_proposal_keeps_identity_and_clears_stale_kickoff() {
        let store = store().await;
        let week = reg_week(16);

        let before = [
            game("xmas", Some(datetime!(2025-12-25 18:00 UTC))),
            game("sun_1", Some(datetime!(2025-12-28 17:00 UTC))),
            game("sun_2", Some(datetime!(2025-12-28 20:25 UTC))),
        ];
        let established = reconcile(
            &store,
            week,
            &[
                proposal(
                    week,
                    "Week 16",
                    Some(datetime!(2025-12-28 17:00 UTC)),
                    &["sun_1", "sun_2"],
                ),
                proposal(
                    week,
                    "Christmas",
                    Some(datetime!(2025-12-25 18:00 UTC)),
                    &["xmas"],
                ),
            ],
            &before,
            false,
        )
        .await
        .expect("establish");
        let christmas_id = established
            .iter()
            .find(|slate| slate.name == "Christmas")
            .expect("christmas slate")
            .id;

        let after = [
            game("xmas", Some(datetime!(2025-12-27 18:00 UTC))),
            game("sun_1", Some(datetime!(2025-12-28 17:00 UTC))),
            game("sun_2", Some(datetime!(2025-12-28 20:25 UTC))),
        ];
        let current = [proposal(
            week,
            "Week 16",
            Some(datetime!(2025-12-28 17:00 UTC)),
            &["sun_1", "sun_2"],
        )];

        let unfixed = reconcile(&store, week, &current, &after, false)
            .await
            .expect("unfixed reconcile");
        let leftover = unfixed
            .iter()
            .find(|slate| slate.name == "Christmas")
            .expect("the omitted row survives");
        assert_eq!(leftover.id, christmas_id, "the assigned id is preserved");
        assert_eq!(leftover.kickoff, None, "the stale kickoff cleared");
        assert_eq!(
            by_id(store.reader(), christmas_id)
                .await
                .expect("by id")
                .expect("stored slate")
                .kickoff,
            None,
            "by-id access agrees with the listing"
        );

        let fixed = reconcile(&store, week, &current, &after, true)
            .await
            .expect("fixing reconcile");
        let fixed_leftover = fixed
            .iter()
            .find(|slate| slate.name == "Christmas")
            .expect("the omitted row survives fixation");
        assert_eq!(fixed_leftover.id, christmas_id);
        assert_eq!(fixed_leftover.kickoff, None);
        assert!(is_fixed(store.reader(), week).await.unwrap());
        assert!(
            game_ids(store.reader(), christmas_id)
                .await
                .unwrap()
                .is_empty(),
            "the omitted proposal gained no membership"
        );

        let later = reconcile(&store, week, &[], &after, false)
            .await
            .expect("fixed refresh");
        let later_leftover = later
            .iter()
            .find(|slate| slate.name == "Christmas")
            .expect("the omitted row survives");
        assert_eq!(later_leftover.id, christmas_id);
        assert_eq!(later_leftover.kickoff, None);
    }

    #[tokio::test]
    async fn failed_snapshot_rolls_back_metadata_and_memberships() {
        let store = store().await;
        let week = reg_week(5);
        let proposals = [
            proposal(
                week,
                "Week 5",
                Some(datetime!(2025-10-05 17:00 UTC)),
                &["a"],
            ),
            proposal(
                week,
                "Week 5",
                Some(datetime!(2025-10-05 17:00 UTC)),
                &["a", "b"],
            ),
        ];

        let err = reconcile(&store, week, &proposals, &[], true)
            .await
            .expect_err("the primary-key violation fails the transaction");
        assert!(
            err.to_string().to_lowercase().contains("unique"),
            "expected a membership primary-key violation, got: {err}"
        );

        assert!(
            for_week(store.reader(), week)
                .await
                .expect("for week")
                .is_empty(),
            "the metadata upserts rolled back"
        );
        assert!(
            !is_fixed(store.reader(), week).await.unwrap(),
            "no membership rows were written"
        );
    }

    #[tokio::test]
    async fn concurrent_file_connections_cannot_replace_snapshot() {
        let dir = tempfile::tempdir().expect("temp dir");
        let url = format!("sqlite:{}", dir.path().join("slates.db").display());
        let store_a = Store::open(&url).await.expect("first store");
        let store_b = Store::open(&url).await.expect("second store");

        let week = reg_week(5);
        let games_a = [
            game("a", Some(datetime!(2025-10-05 17:00 UTC))),
            game("b", Some(datetime!(2025-10-05 20:25 UTC))),
        ];
        let games_b = [
            game("c", Some(datetime!(2025-10-05 17:00 UTC))),
            game("d", Some(datetime!(2025-10-05 20:25 UTC))),
        ];
        let proposals_a = [proposal(
            week,
            "Week 5",
            Some(datetime!(2025-10-05 17:00 UTC)),
            &["a", "b"],
        )];
        let proposals_b = [proposal(
            week,
            "Week 5",
            Some(datetime!(2025-10-05 17:00 UTC)),
            &["c", "d"],
        )];

        let (from_a, from_b) = tokio::join!(
            reconcile(&store_a, week, &proposals_a, &games_a, true),
            reconcile(&store_b, week, &proposals_b, &games_b, true),
        );
        let from_a = from_a.expect("first reconcile");
        let from_b = from_b.expect("second reconcile");

        assert!(is_fixed(store_a.reader(), week).await.unwrap());
        assert_eq!(
            for_week(store_a.reader(), week)
                .await
                .expect("for week")
                .len(),
            1,
            "one slate, not a union"
        );
        assert_eq!(from_a.len(), 1);
        assert_eq!(from_b.len(), 1);
        assert_eq!(
            from_a[0].id, from_b[0].id,
            "both connections report one winning snapshot, not a replacement"
        );

        let id = from_a[0].id;
        let got = game_ids(store_a.reader(), id).await.expect("membership");
        assert!(
            got == vec!["a", "b"] || got == vec!["c", "d"],
            "one complete winning snapshot, not a union: {got:?}"
        );

        let later = [proposal(week, "Week 5", None, &["e", "f"])];
        reconcile(&store_b, week, &later, &[], false)
            .await
            .expect("later reconcile");
        assert_eq!(
            game_ids(store_a.reader(), id).await.expect("membership"),
            got,
            "later proposals did not change the snapshot"
        );
    }

    #[tokio::test]
    async fn migration_preserves_existing_player_links() {
        const PLAYER_LINKS_UP: &str =
            include_str!("../../migrations/20261027000000_create_player_links.up.sql");
        const PLAYER_LINKS_DOWN: &str =
            include_str!("../../migrations/20261027000000_create_player_links.down.sql");

        let dir = tempfile::tempdir().expect("temp dir");
        let migrations = dir.path().join("migrations");
        std::fs::create_dir(&migrations).expect("migrations dir");
        std::fs::write(
            migrations.join("20261027000000_create_player_links.up.sql"),
            PLAYER_LINKS_UP,
        )
        .expect("write up migration");
        std::fs::write(
            migrations.join("20261027000000_create_player_links.down.sql"),
            PLAYER_LINKS_DOWN,
        )
        .expect("write down migration");

        let url = format!("sqlite:{}", dir.path().join("store.db").display());
        let legacy = sqlx::SqlitePool::connect_with(Store::options(&url).expect("options"))
            .await
            .expect("legacy pool");
        sqlx::migrate::Migrator::new(migrations.as_path())
            .await
            .expect("build migrator")
            .run(&legacy)
            .await
            .expect("legacy migration");

        let legacy_store = Store {
            read: legacy.clone(),
            write: legacy,
        };
        save_links(
            &legacy_store,
            &[PlayerLink {
                fd_player_id: 1,
                gsis_player_id: "00-1".into(),
            }],
        )
        .await
        .expect("seed link");
        drop(legacy_store);

        let store = Store::open(&url).await.expect("store with full migrator");
        assert_eq!(
            links(store.reader()).await.expect("links"),
            HashMap::from([(1, "00-1".to_owned())]),
            "the stored link survives the slate migration"
        );
        assert!(
            for_week(store.reader(), reg_week(1))
                .await
                .expect("slate schema")
                .is_empty(),
            "the slate migration created its tables"
        );
    }
}
