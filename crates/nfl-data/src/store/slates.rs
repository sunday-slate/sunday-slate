//! Durable slate metadata and atomic weekly membership snapshots.
//!
//! Slate metadata is keyed by its exact [`SeasonWeek`] and canonical name,
//! with the proposal's derived [`SlateId`] as its primary key. A week is
//! fixed iff membership rows exist for it — fixation is inferred from
//! membership presence joined through `slates.season_week`, with no separate
//! marker table. All writes run inside one [`Store::write_tx`] transaction:
//! either the whole snapshot (metadata, memberships) lands or nothing does.
//! Once a week is fixed, replacement proposals are ignored — the snapshot is
//! the durable record. Metadata rows and membership game ids carry no foreign
//! key to the disposable provider database: the slate tables are the only
//! durable side.

#![cfg_attr(not(test), expect(dead_code))]

use std::collections::HashMap;

use sqlx::{Executor, Sqlite, SqliteConnection, SqlitePool};
use time::{OffsetDateTime, UtcOffset};

use crate::slates::{Slate, SlateId, SlateProposal};
use crate::store::Store;
use crate::{Game, SeasonWeek};

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
               ORDER BY kickoff IS NULL, kickoff, id"#,
    )
    .bind(week)
    .fetch_all(ex)
    .await
}

/// The stored slate with this id, or `None` when absent.
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

/// Whether the week's membership snapshot is fixed: true iff membership rows
/// exist for the week, joined through `slates.season_week`.
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
                   FROM slate_games
                   JOIN slates ON slates.id = slate_games.slate_id
                   WHERE slates.season_week = ?)"#,
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

/// Reconcile one NFL week inside a single `BEGIN IMMEDIATE` transaction.
///
/// Fixation is rechecked inside the transaction by probing membership
/// presence, so concurrent writers serialize: the first fixation wins and
/// every later one sees it. Before fixation, each proposal's canonical
/// metadata is upserted with its derived id, stored slates absent from the
/// proposal set keep their ids but clear their stale kickoffs, and no
/// tentative membership rows are stored. When `fix` is true, every
/// proposal's membership rows are inserted in that same transaction. After
/// fixation, proposals are ignored, no new memberships are created, and only
/// the stored kickoffs are refreshed from the fixed ids and the supplied
/// current schedule.
///
/// Returns the week's metadata ordered by known kickoff ascending, unknown
/// kickoff last, id breaking ties.
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

/// The transaction body of [`reconcile`], which owns the `write_tx`.
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
        if fix {
            for proposal in proposals {
                for game_id in &proposal.game_ids {
                    sqlx::query("INSERT INTO slate_games (slate_id, gsis_game_id) VALUES (?, ?)")
                        .bind(proposal.id)
                        .bind(game_id)
                        .execute(&mut *conn)
                        .await?;
                }
            }
        }
    }
    for_week_on(&mut *conn, week).await
}

/// Upsert one proposal's canonical metadata row with its derived id: on
/// `(season_week, name)` conflict the kickoff updates to the proposal's value.
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

/// Clear the kickoffs of the week's stored slates absent from the current
/// proposal set: a proposal that disappeared — for example a holiday whose
/// last matching game rescheduled off the holiday date — keeps its stored
/// identity but drops its stale kickoff, so identical schedule data yields
/// identical metadata on every lookup, including the first fixing one.
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

/// Refresh each stored kickoff from the slate's fixed member ids resolved
/// against the supplied schedule: the earliest known member kickoff wins, or
/// `None` when no member has a known kickoff.
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

    /// A fixture game with irrelevant teams and no scores: storage reads only
    /// the gsis id and the kickoff.
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

    /// A proposal carrying the derived id the storage reconciliation stores:
    /// `week.encode() * 10 + kind`, mirroring the pure policy's stamping,
    /// with the kind digit from the canonical name.
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

    /// The id kind digit a canonical name maps to: `0` for the holiday
    /// slates, `1` for main slates, mirroring the pure policy.
    fn kind(name: &str) -> i64 {
        match name {
            "Thanksgiving" | "Christmas" => 0,
            _ => 1,
        }
    }

    /// A stored slate round-trips all four public fields through the
    /// database — including a `None` kickoff — with no provider or games
    /// involved.
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

        // Reconstruct the slate from its row through `by_id` alone.
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

    /// Reconciling the same canonical proposals repeatedly yields identical
    /// derived ids; REG 2025 week 13's metadata rows carry exactly the
    /// derived values; and other weeks or names yield their own derived ids.
    #[tokio::test]
    async fn repeated_reconciliation_preserves_assigned_ids() {
        let store = store().await;

        // Repeated reconciliation yields identical ids.
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

        // REG 2025 week 13: the metadata rows carry exactly the derived
        // values — holiday kind 0 and main kind 1.
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

        // Different weeks or names yield their own derived ids.
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

    /// `for_week` orders known kickoffs ascending, unknown kickoffs last, and
    /// breaks equal kickoffs by id; identical numeric weeks stay distinct
    /// across season types.
    #[tokio::test]
    async fn metadata_queries_are_week_scoped_and_chronological() {
        let store = store().await;

        // Known kickoffs ascending: names are storage labels here, and the
        // fixture gives the main slate the earlier kickoff and the holiday
        // the later one — the reverse of id order — so the listing order
        // proves kickoff ordering, not id ordering.
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

        // The tie pairs two equal kickoffs; the derived ids break it
        // deterministically: the holiday kind sorts before the main kind.
        // The main label sorts before the holiday alphabetically, so the
        // listing order proves id ordering, not name ordering.
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

        // Unknown kickoff is last: the unknown kickoff sits on the holiday,
        // whose derived id sorts first — the reverse of id order — so the
        // listing order proves the nulls-last rule, not id ordering.
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

        // The same numeric week under another season type is a separate set.
        reconcile(
            &store,
            reg_week(19),
            &[proposal(reg_week(19), "Week 19", None, &[])],
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
        let reg = for_week(store.reader(), reg_week(19))
            .await
            .expect("reg week");
        let post = for_week(store.reader(), post_week(19))
            .await
            .expect("post week");
        assert_eq!(
            reg.iter()
                .map(|slate| slate.name.as_str())
                .collect::<Vec<_>>(),
            ["Week 19"]
        );
        assert_eq!(
            post.iter()
                .map(|slate| slate.name.as_str())
                .collect::<Vec<_>>(),
            ["Wild Card"]
        );
        assert_ne!(
            reg[0].id, post[0].id,
            "REG and POST with the same numeric week stay distinct"
        );
    }

    /// A week with stored metadata but no membership rows reads unfixed:
    /// fixation is inferred from membership presence, and tentative
    /// reconciliation persists no membership rows.
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

    /// Once the week is fixed, replacement proposals cannot rewrite the
    /// stored membership snapshot.
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

        // The week now offers a replacement membership.
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

    /// After fixation the stored kickoff follows the earliest current kickoff
    /// of the fixed members, never a replacement proposal's new match.
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

        // `a` moved to a later slot and a new, earlier game `c` matched the
        // rule but is not a member.
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

    /// A stored slate absent from the current proposal set keeps its assigned
    /// id but loses its stale kickoff — on the unfixed lookup and on the
    /// first fixing lookup alike — so identical schedule data yields
    /// identical metadata on every lookup.
    #[tokio::test]
    async fn omitted_proposal_keeps_identity_and_clears_stale_kickoff() {
        let store = store().await;
        let week = reg_week(16);

        // The tentative establishment: the Christmas slate beside the main
        // slate, both with known kickoffs.
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

        // The holiday's last game reschedules off December 25, so the current
        // proposal set no longer contains Christmas.
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

        // The unfixed reconciliation clears the stale kickoff without
        // deleting the row.
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

        // The first fixing lookup reports the same metadata: no membership is
        // inserted for the omitted proposal, and the stale kickoff stays
        // cleared.
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

        // Later fixed lookups agree with the first fixing lookup.
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

    /// A membership failure aborts the whole reconciliation transaction: no
    /// metadata and no memberships survive.
    #[tokio::test]
    async fn failed_snapshot_rolls_back_metadata_and_memberships() {
        let store = store().await;
        let week = reg_week(5);
        // Two proposals sharing the canonical name resolve to one slate row,
        // so the second proposal's overlapping game id violates the
        // (slate_id, gsis_game_id) primary key.
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

    /// Two writer connections on one file serialize through BEGIN IMMEDIATE:
    /// the losing reconcile sees the committed snapshot and cannot replace or
    /// extend it.
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

        // Later proposals from either connection leave the snapshot alone.
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

    /// SQLx migration bookkeeping survives the slate migration: a link stored
    /// before `Store` runs the full migrator is untouched by it.
    #[tokio::test]
    async fn migration_preserves_existing_player_links() {
        const PLAYER_LINKS_UP: &str =
            include_str!("../../migrations/20261027000000_create_player_links.up.sql");
        const PLAYER_LINKS_DOWN: &str =
            include_str!("../../migrations/20261027000000_create_player_links.down.sql");

        let dir = tempfile::tempdir().expect("temp dir");
        // A temporary migrations directory holding only the unchanged
        // existing player-link pair: the pre-slate migration state real
        // deployments carry.
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

        // Add a link through the real write path on the legacy schema.
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

        // Opening the store runs the full migrator: the bookkeeping already
        // marks the player-link migration applied, so only the slate
        // migration runs.
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
