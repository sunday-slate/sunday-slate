//! The admin's pending-setup banner: the next contest that is not ready.

use time::OffsetDateTime;

use crate::contests::rule;
use crate::contests::service::SeasonSlates;
use crate::contests::{Contest, ContestId};
use crate::player_salaries::store as player_salaries;
use crate::{AppError, AppState};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminBanner {
    pub contest: ContestId,
    pub name: String,
    /// No committed salaries exist for the contest's games.
    pub needs_salaries: bool,
    /// No published slate exists for the contest yet.
    pub needs_slate: bool,
}

/// The soonest upcoming contest that is not ready, plus which tasks remain.
/// A contest's games come from its published slate when one exists, or from
/// its rule-proposed slate otherwise. Ready contests (published slate,
/// committed salaries) are skipped rather than masking a later contest that
/// still needs work. A contest only nags from the Monday of its game week.
/// `None` when every upcoming contest is ready, none exist, or the next
/// not-ready contest's week has not begun.
pub async fn pending(
    state: &AppState,
    slates: &SeasonSlates,
    contests: &[Contest],
    now: OffsetDateTime,
) -> Result<Option<AdminBanner>, AppError> {
    let season = state.config.season;
    let kickoffs = slates.kickoffs();

    let mut candidates: Vec<Candidate<'_>> = Vec::new();
    for contest in contests {
        let (games, needs_slate) = slate_games(slates, season, contest);
        let Some(first) = games
            .iter()
            .filter_map(|id| kickoffs.get(id).copied())
            .min()
        else {
            continue;
        };
        // Nag only from the Monday of the contest's game week: reminding
        // earlier is noise — slates and salaries firm up in the days right
        // before the week.
        if first > now && now.date() >= crate::contests::service::nfl_week_monday(first.date()) {
            candidates.push(Candidate {
                first,
                games,
                needs_slate,
                contest,
            });
        }
    }
    candidates.sort_by_key(|c| c.first);

    for candidate in candidates {
        let contest = candidate.contest;
        let needs_salaries =
            !player_salaries::any_for_games(state.db.reader(), &candidate.games).await?;
        if candidate.needs_slate || needs_salaries {
            return Ok(Some(AdminBanner {
                contest: contest.id,
                name: contest.name.clone(),
                needs_salaries,
                needs_slate: candidate.needs_slate,
            }));
        }
    }
    Ok(None)
}

/// An upcoming contest with a dated slate, waiting on the reminder window.
struct Candidate<'a> {
    /// The slate's earliest kickoff.
    first: OffsetDateTime,
    /// The slate's gsis game ids, borrowed from the loaded schedule.
    games: Vec<&'a str>,
    /// The games are only a rule's proposal — nothing is published yet.
    needs_slate: bool,
    contest: &'a Contest,
}

/// A contest's slate as gsis ids, and whether it still needs publishing:
/// its published slate when one exists, otherwise what its rule proposes.
/// Empty when neither applies.
fn slate_games<'a>(
    slates: &'a SeasonSlates,
    season: u16,
    contest: &Contest,
) -> (Vec<&'a str>, bool) {
    if let Some(published) = slates.slates.get(&contest.id) {
        return (published.iter().map(|g| g.0.as_str()).collect(), false);
    }
    let Some(rule) = contest.rule() else {
        return (Vec::new(), true);
    };
    let games = slates
        .games
        .iter()
        .filter(|g| rule::covers(&rule, season, g))
        .map(|g| g.gsis_game_id.as_str())
        .collect();
    (games, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::TestApp;
    use crate::tests::factories;
    use nfl_data::Season;
    use sqlx::SqlitePool;
    use time::macros::datetime;

    /// Load the season and run `pending` at the app's clock.
    async fn card(app: &TestApp, pool: &SqlitePool) -> Option<AdminBanner> {
        let slates = SeasonSlates::load(pool, &app.nfl, Season(2025))
            .await
            .unwrap();
        let contests = crate::contests::store::all(pool).await.unwrap();
        pending(&app.state(), &slates, &contests, app.state().now())
            .await
            .unwrap()
    }

    async fn seed_salary(pool: &SqlitePool, game_id: &str) {
        sqlx::query(
            "INSERT INTO nfl_player_salaries (gsis_game_id, gsis_player_id, team_abbr, dfs_position, salary)
             VALUES (?1, '00-A', 'BUF', 'RB', 7000)",
        )
        .bind(game_id)
        .execute(pool)
        .await
        .unwrap();
    }

    #[sqlx::test]
    async fn card_for_the_next_unpublished_contest(pool: SqlitePool) {
        let app = TestApp::from_pool_at(pool.clone(), datetime!(2025-09-06 12:00 UTC)).await;
        let g = factories::game_at("2025_01_BUF_KC", 1, datetime!(2025-09-07 17:00 UTC));
        app.nfl.seed_for_test(&[], &[g]).await.unwrap();
        factories::contest(&pool, "Week 1").await;

        let card = card(&app, &pool)
            .await
            .expect("an unpublished contest with a future kickoff");
        assert!(card.needs_salaries, "no salaries seeded");
        assert!(card.needs_slate, "the slate is unpublished");
        assert_eq!(card.name, "Week 1");
    }

    #[sqlx::test]
    async fn salaries_present_clears_the_salary_task(pool: SqlitePool) {
        let app = TestApp::from_pool_at(pool.clone(), datetime!(2025-09-06 12:00 UTC)).await;
        let g = factories::game_at("2025_01_BUF_KC", 1, datetime!(2025-09-07 17:00 UTC));
        app.nfl.seed_for_test(&[], &[g]).await.unwrap();
        factories::contest(&pool, "Week 1").await;
        seed_salary(&pool, "2025_01_BUF_KC").await;

        let card = card(&app, &pool)
            .await
            .expect("an unpublished contest with a future kickoff");
        assert!(!card.needs_salaries, "salaries were seeded");
        assert!(card.needs_slate, "the slate is still unpublished");
    }

    #[sqlx::test]
    async fn card_for_a_published_slate_missing_salaries(pool: SqlitePool) {
        let app = TestApp::from_pool_at(pool.clone(), datetime!(2025-09-06 12:00 UTC)).await;
        let g = factories::game_at("2025_01_BUF_KC", 1, datetime!(2025-09-07 17:00 UTC));
        app.nfl.seed_for_test(&[], &[g]).await.unwrap();
        factories::contest_with_games(&pool, "Week 1", &["2025_01_BUF_KC"]).await;

        let card = card(&app, &pool)
            .await
            .expect("a published slate with no committed salaries");
        assert!(card.needs_salaries, "no salaries seeded");
        assert!(!card.needs_slate, "the slate is already published");
    }

    #[sqlx::test]
    async fn no_card_when_the_published_slate_is_salaried(pool: SqlitePool) {
        let app = TestApp::from_pool_at(pool.clone(), datetime!(2025-09-06 12:00 UTC)).await;
        let g = factories::game_at("2025_01_BUF_KC", 1, datetime!(2025-09-07 17:00 UTC));
        app.nfl.seed_for_test(&[], &[g]).await.unwrap();
        factories::contest_with_games(&pool, "Week 1", &["2025_01_BUF_KC"]).await;
        seed_salary(&pool, "2025_01_BUF_KC").await;

        assert!(
            card(&app, &pool).await.is_none(),
            "the slate is published and salaried"
        );
    }

    #[sqlx::test]
    async fn no_card_before_the_monday_of_the_contest_week(pool: SqlitePool) {
        // Saturday Sep 6: Week 2's slate (Sun Sep 14) is still a week out.
        let app = TestApp::from_pool_at(pool.clone(), datetime!(2025-09-06 12:00 UTC)).await;
        let g = factories::game_at("2025_02_BUF_KC", 2, datetime!(2025-09-14 17:00 UTC));
        app.nfl.seed_for_test(&[], &[g]).await.unwrap();
        factories::contest(&pool, "Week 2").await;

        assert!(
            card(&app, &pool).await.is_none(),
            "week 2's reminder waits for its Monday"
        );
    }

    #[sqlx::test]
    async fn card_appears_on_the_monday_of_the_contest_week(pool: SqlitePool) {
        // Monday Sep 8: Week 2's slate (Sun Sep 14) is now this week.
        let app = TestApp::from_pool_at(pool.clone(), datetime!(2025-09-08 12:00 UTC)).await;
        let g = factories::game_at("2025_02_BUF_KC", 2, datetime!(2025-09-14 17:00 UTC));
        app.nfl.seed_for_test(&[], &[g]).await.unwrap();
        factories::contest(&pool, "Week 2").await;

        let card = card(&app, &pool).await.expect("the week has begun");
        assert_eq!(card.name, "Week 2");
    }

    #[sqlx::test]
    async fn no_card_when_the_window_has_passed(pool: SqlitePool) {
        // Two days after the proposed kickoff.
        let app = TestApp::from_pool_at(pool.clone(), datetime!(2025-09-09 12:00 UTC)).await;
        let g = factories::game_at("2025_01_BUF_KC", 1, datetime!(2025-09-07 17:00 UTC));
        app.nfl.seed_for_test(&[], &[g]).await.unwrap();
        factories::contest(&pool, "Week 1").await;

        assert!(
            card(&app, &pool).await.is_none(),
            "the window already passed"
        );
    }

    #[sqlx::test]
    async fn soonest_unpublished_contest_wins(pool: SqlitePool) {
        let app = TestApp::from_pool_at(pool.clone(), datetime!(2025-09-06 12:00 UTC)).await;
        let g1 = factories::game_at("2025_01_BUF_KC", 1, datetime!(2025-09-07 17:00 UTC));
        let g2 = factories::game_at("2025_02_BUF_KC", 2, datetime!(2025-09-14 17:00 UTC));
        app.nfl.seed_for_test(&[], &[g1, g2]).await.unwrap();
        factories::contest(&pool, "Week 2").await;
        factories::contest(&pool, "Week 1").await;

        let card = card(&app, &pool)
            .await
            .expect("the soonest unpublished contest");
        assert_eq!(card.name, "Week 1");
    }

    #[sqlx::test]
    async fn a_ready_contest_does_not_mask_a_later_not_ready_one(pool: SqlitePool) {
        // Monday Sep 8: Week 1 opens Thursday, Week 2 the following Sunday,
        // so both reminder windows are open.
        let app = TestApp::from_pool_at(pool.clone(), datetime!(2025-09-08 12:00 UTC)).await;
        let g1 = factories::game_at("2025_01_BUF_KC", 1, datetime!(2025-09-12 00:15 UTC)); // Thu 8:15 PM ET
        let g2 = factories::game_at("2025_02_BUF_KC", 2, datetime!(2025-09-14 17:00 UTC));
        app.nfl.seed_for_test(&[], &[g1, g2]).await.unwrap();

        // Week 1 is the soonest contest and fully ready: published + salaried.
        factories::contest_with_games(&pool, "Week 1", &["2025_01_BUF_KC"]).await;
        seed_salary(&pool, "2025_01_BUF_KC").await;

        // Week 2 kicks off later and is still unpublished.
        factories::contest(&pool, "Week 2").await;

        let card = card(&app, &pool).await.expect("Week 2 still needs a slate");
        assert_eq!(card.name, "Week 2");
        assert!(card.needs_slate);
    }
}
