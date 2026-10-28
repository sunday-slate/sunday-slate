use crate::slates::{Slate, SlateError, SlateId, rule};
use crate::{NflData, store};
use nfl_model::{Game, SeasonWeek, to_eastern};
use std::collections::HashMap;
use time::OffsetDateTime;

/// The week's stored slates, ordered by known kickoff ascending, unknown
/// kickoff last, id breaking ties.
///
/// Tentative before the week's opening Monday (Eastern), with kickoffs
/// following the current schedule; the first lookup at or after that
/// Monday durably fixes the membership snapshot, and later lookups only
/// refresh stored kickoffs from the fixed members.
pub async fn by_week(nfl: &NflData, week: SeasonWeek) -> Result<Vec<Slate>, SlateError> {
    slates_at(nfl, week, OffsetDateTime::now_utc()).await
}

/// The stored slate with this id, or `None` when the id is unknown.
///
/// A known id resolves its week exactly like [`Self::slates`]; unknown
/// ids have no side effects.
pub async fn by_id(nfl: &NflData, id: SlateId) -> Result<Option<Slate>, SlateError> {
    slate_at(nfl, id, OffsetDateTime::now_utc()).await
}

/// The slate's games, ordered by known kickoff ascending, unknown kickoff
/// last, game id breaking ties. Unknown ids have no games.
///
/// After fixation the membership is the stored snapshot resolved against
/// the current schedule; before fixation it is the current matching
/// proposal. Rescheduled games keep their slate and current kickoff, and
/// games whose schedule row is absent drop out of the list without
/// leaving the snapshot.
pub async fn games(nfl: &NflData, id: SlateId) -> Result<Vec<Game>, SlateError> {
    slate_games_at(nfl, id, OffsetDateTime::now_utc()).await
}

pub(crate) async fn slates_at(
    nfl: &NflData,
    week: SeasonWeek,
    now: OffsetDateTime,
) -> Result<Vec<Slate>, SlateError> {
    let (slates, _) = resolve_week_at(nfl, week, now).await?;
    Ok(slates)
}

pub(crate) async fn slate_at(
    nfl: &NflData,
    id: SlateId,
    now: OffsetDateTime,
) -> Result<Option<Slate>, SlateError> {
    let Some(stored) = store::slates::by_id(nfl.store.reader(), id).await? else {
        return Ok(None);
    };
    let (slates, _) = resolve_week_at(nfl, stored.season_week, now).await?;
    Ok(slates.into_iter().find(|slate| slate.id == id))
}

pub(crate) async fn slate_games_at(
    nfl: &NflData,
    id: SlateId,
    now: OffsetDateTime,
) -> Result<Vec<Game>, SlateError> {
    let Some(stored) = store::slates::by_id(nfl.store.reader(), id).await? else {
        return Ok(Vec::new());
    };
    let (_, schedule) = resolve_week_at(nfl, stored.season_week, now).await?;
    if store::slates::is_fixed(nfl.store.reader(), stored.season_week).await? {
        let ids = store::slates::game_ids(nfl.store.reader(), id).await?;
        return Ok(selected_games(&schedule, &ids));
    }
    let proposal = rule::proposals(stored.season_week, &schedule)
        .into_iter()
        .find(|proposal| proposal.name == stored.name);
    Ok(match proposal {
        Some(proposal) => selected_games(&schedule, &proposal.game_ids),
        None => Vec::new(),
    })
}

/// Reconciles the week once and returns its stored rows plus the loaded
/// schedule for reuse; a fixed week's stored snapshot wins regardless of
/// the clock.
async fn resolve_week_at(
    nfl_data: &NflData,
    week: SeasonWeek,
    now: OffsetDateTime,
) -> Result<(Vec<Slate>, Vec<Game>), SlateError> {
    let schedule = nfl_data.provider.games(week.season).await?;
    let slates = if store::slates::is_fixed(nfl_data.store.reader(), week).await? {
        store::slates::reconcile(&nfl_data.store, week, &[], &schedule, false).await?
    } else {
        let week_proposals = rule::proposals(week, &schedule);
        let fix = rule::opening_monday(week, &schedule)
            .is_some_and(|monday| to_eastern(now).date() >= monday);
        store::slates::reconcile(&nfl_data.store, week, &week_proposals, &schedule, fix).await?
    };
    Ok((slates, schedule))
}

/// The schedule's games for `ids`, ordered by known kickoff ascending,
/// unknown last, id breaking ties; absent ids contribute nothing.
fn selected_games(schedule: &[Game], ids: &[String]) -> Vec<Game> {
    let by_id: HashMap<&str, &Game> = schedule
        .iter()
        .map(|game| (game.gsis_game_id.as_str(), game))
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
            .then_with(|| a.gsis_game_id.cmp(&b.gsis_game_id))
    });
    selected
}
