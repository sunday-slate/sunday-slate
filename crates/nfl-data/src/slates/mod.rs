//! Shared, named slates: public metadata types, the pure default membership
//! policy, and the `NflData` facade that lazily establishes and fixes weekly
//! snapshots.

use std::collections::HashMap;

use time::OffsetDateTime;

use self::rule::{opening_monday, proposals};
use crate::store;
use crate::{Game, NflData, SeasonWeek, to_eastern};

mod error;
mod model;
mod rule;

pub use error::SlateError;
pub use model::{Slate, SlateId};

pub(crate) use model::SlateProposal;

#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests;

impl NflData {
    /// The week's stored slates, ordered by known kickoff ascending, unknown
    /// kickoff last, id breaking ties.
    ///
    /// The first lookup for a week lazily establishes it: before the week's
    /// opening Monday (Eastern) the canonical slates exist as tentative
    /// metadata whose kickoffs follow the current schedule, and the first
    /// lookup at or after that Monday fixes the membership snapshot durably.
    /// Once fixed, later lookups refresh stored kickoffs from the fixed
    /// members' current schedule; they never change membership.
    pub async fn slates(&self, week: SeasonWeek) -> Result<Vec<Slate>, SlateError> {
        self.slates_at(week, OffsetDateTime::now_utc()).await
    }

    /// The stored slate with this id, or `None` when the id is unknown.
    ///
    /// A known id resolves its week exactly like [`Self::slates`]: the first
    /// lookup at or after the week's opening Monday (Eastern) fixes that
    /// week's snapshot. Unknown ids have no side effects.
    pub async fn slate(&self, id: SlateId) -> Result<Option<Slate>, SlateError> {
        self.slate_at(id, OffsetDateTime::now_utc()).await
    }

    /// The slate's games, ordered by known kickoff ascending, unknown kickoff
    /// last, game id breaking ties. Unknown ids have no games.
    ///
    /// After fixation the membership is the stored snapshot resolved against
    /// the current schedule: a rescheduled game keeps its slate and carries
    /// its current kickoff, and a game whose schedule row is absent drops out
    /// of the returned list without leaving the snapshot. Before fixation the
    /// membership is the current matching proposal.
    pub async fn slate_games(&self, id: SlateId) -> Result<Vec<Game>, SlateError> {
        self.slate_games_at(id, OffsetDateTime::now_utc()).await
    }

    /// The week's stored slates at `now`, reconciling the week first.
    async fn slates_at(
        &self,
        week: SeasonWeek,
        now: OffsetDateTime,
    ) -> Result<Vec<Slate>, SlateError> {
        let (slates, _) = self.resolve_week_at(week, now).await?;
        Ok(slates)
    }

    /// The stored slate with this id at `now`, or `None` without side effects
    /// when the id is unknown.
    async fn slate_at(
        &self,
        id: SlateId,
        now: OffsetDateTime,
    ) -> Result<Option<Slate>, SlateError> {
        let Some(stored) = store::slates::by_id(self.store.reader(), id).await? else {
            return Ok(None);
        };
        let (slates, _) = self.resolve_week_at(stored.season_week, now).await?;
        Ok(slates.into_iter().find(|slate| slate.id == id))
    }

    /// The slate's games at `now`: the stored snapshot after fixation, the
    /// current matching proposal before it. Unknown ids have no games and no
    /// side effects.
    async fn slate_games_at(
        &self,
        id: SlateId,
        now: OffsetDateTime,
    ) -> Result<Vec<Game>, SlateError> {
        let Some(stored) = store::slates::by_id(self.store.reader(), id).await? else {
            return Ok(Vec::new());
        };
        let (_, schedule) = self.resolve_week_at(stored.season_week, now).await?;
        if store::slates::is_fixed(self.store.reader(), stored.season_week).await? {
            let ids = store::slates::game_ids(self.store.reader(), id).await?;
            return Ok(selected_games(&schedule, &ids));
        }
        let proposal = proposals(stored.season_week, &schedule)
            .into_iter()
            .find(|proposal| proposal.name == stored.name);
        Ok(match proposal {
            Some(proposal) => selected_games(&schedule, &proposal.game_ids),
            None => Vec::new(),
        })
    }

    /// Reconcile one exact week against the season schedule loaded once, then
    /// return the week's stored rows and that schedule for reuse. The week
    /// fixes on the first lookup at or after its opening Monday in Eastern
    /// time; an existing snapshot always wins regardless of the clock.
    async fn resolve_week_at(
        &self,
        week: SeasonWeek,
        now: OffsetDateTime,
    ) -> Result<(Vec<Slate>, Vec<Game>), SlateError> {
        let schedule = self.provider.games(week.season).await?;
        let week_proposals = proposals(week, &schedule);
        let fix =
            opening_monday(week, &schedule).is_some_and(|monday| to_eastern(now).date() >= monday);
        let slates =
            store::slates::reconcile(&self.store, week, &week_proposals, &schedule, fix).await?;
        Ok((slates, schedule))
    }
}

/// The current schedule's games for `ids`, ordered by known kickoff ascending,
/// unknown kickoff last, game id breaking ties. Ids absent from the schedule
/// contribute nothing.
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
