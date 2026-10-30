use crate::slates::{Slate, SlateError, SlateId, rule};
use crate::store::Store;
use crate::store::slates as slates_store;
use crate::{NflData, NflverseData};
use nfl_model::{Game, Season, SeasonWeek, to_eastern};
use std::collections::{BTreeSet, HashMap};
use time::OffsetDateTime;

/// Every stored slate for the handle's season, in ascending order.
pub async fn all(nfl: &NflData) -> Result<BTreeSet<Slate>, SlateError> {
    Ok(slates_store::for_season(nfl.store.reader(), nfl.season())
        .await?
        .into_iter()
        .collect())
}

/// The week's stored slates, ordered by kickoff
pub async fn by_week(nfl: &NflData, week: SeasonWeek) -> Result<Vec<Slate>, SlateError> {
    Ok(slates_store::for_week(nfl.store.reader(), week).await?)
}

/// The stored slate with this id, or `None` when the id is unknown.
pub async fn by_id(nfl: &NflData, id: SlateId) -> Result<Option<Slate>, SlateError> {
    Ok(slates_store::by_id(nfl.store.reader(), id).await?)
}

/// The slate's games, ordered by known kickoff ascending, unknown kickoff
/// last, game id breaking ties.
///
/// Membership is the stored snapshot resolved against the current schedule, so
/// rescheduled games keep their slate and current kickoff. Games whose schedule
/// row is absent drop out without leaving the snapshot, and an id with no
/// stored membership has no games.
pub async fn games(nfl: &NflData, id: SlateId) -> Result<Vec<Game>, SlateError> {
    let Some(stored) = slates_store::by_id(nfl.store.reader(), id).await? else {
        return Ok(Vec::new());
    };
    let schedule = nfl.provider.games(stored.season_week.season).await?;
    let ids = slates_store::game_ids(nfl.store.reader(), id).await?;
    Ok(selected_games(&schedule, &ids))
}

/// Games per stored slate for the handle's season; slates with no membership
/// count zero.
pub(crate) async fn counts(nfl: &NflData) -> Result<HashMap<SlateId, usize>, SlateError> {
    Ok(slates_store::counts_for_season(nfl.store.reader(), nfl.season()).await?)
}

/// Reconcile every week of the handle's season; the only writer of slate rows.
///
/// Callers that already hold populated tables do not need this: reads answer
/// from storage, and the handle populates at startup and after each successful
/// sync. It exists for a deliberate rebuild.
pub async fn refresh(nfl: &NflData) -> Result<BTreeSet<Slate>, SlateError> {
    populate_at(
        &nfl.provider,
        &nfl.store,
        nfl.season(),
        OffsetDateTime::now_utc(),
    )
    .await
}

/// [`refresh`] against explicitly supplied parts and clock, for the background
/// worker, which cannot borrow the handle, and for tests that pin the fixation
/// boundary.
pub(crate) async fn populate_at(
    provider: &NflverseData,
    store: &Store,
    season: Season,
    now: OffsetDateTime,
) -> Result<BTreeSet<Slate>, SlateError> {
    let schedule = provider.games(season).await?;
    let mut slates = BTreeSet::new();
    for week in SeasonWeek::for_season(season) {
        slates.extend(reconcile_week_at(store, week, &schedule, now).await?);
    }
    Ok(slates)
}

async fn reconcile_week_at(
    store: &Store,
    week: SeasonWeek,
    schedule: &[Game],
    now: OffsetDateTime,
) -> Result<Vec<Slate>, SlateError> {
    if slates_store::is_fixed(store.reader(), week).await? {
        return Ok(slates_store::reconcile(store, week, &[], schedule, false).await?);
    }
    let proposals = rule::proposals(week, schedule);
    let fix =
        rule::opening_monday(week, schedule).is_some_and(|monday| to_eastern(now).date() >= monday);
    Ok(slates_store::reconcile(store, week, &proposals, schedule, fix).await?)
}

/// The schedule's games for `ids`, ordered by known kickoff ascending,
/// unknown last, id breaking ties; absent ids contribute nothing.
fn selected_games(schedule: &[Game], ids: &[String]) -> Vec<Game> {
    let by_id: HashMap<&str, &Game> = schedule
        .iter()
        .map(|game| (game.game_id.as_str(), game))
        .collect();
    let mut selected: Vec<Game> = ids
        .iter()
        .filter_map(|id| by_id.get(id.as_str()).map(|game| (*game).clone()))
        .collect();
    selected.sort_by(|a, b| {
        a.kickoff
            .is_none()
            .cmp(&b.kickoff.is_none())
            .then_with(|| a.kickoff.cmp(&b.kickoff))
            .then_with(|| a.game_id.cmp(&b.game_id))
    });
    selected
}
