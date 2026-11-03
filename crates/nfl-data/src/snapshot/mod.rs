use crate::salaried_players::{self, SalariedPlayer, SalariedPlayersBySlot};
use crate::slates::{self, Slate, SlateId};
use crate::{NflData, NflDataError};
use nfl_model::DfsPositionSlotType;
use std::collections::{BTreeMap, HashMap};

#[derive(Default)]
pub struct Snapshot {
    slates: BTreeMap<SlateId, Slate>,
    salaried_players: HashMap<SlateId, SalariedPlayersBySlot>,
}

impl Snapshot {
    pub(crate) async fn build(nfl: &NflData) -> Result<Self, NflDataError> {
        let all_slates: Vec<Slate> = slates::all(nfl).await?.into_iter().collect();

        // stored slates exist for every week, even those without games or
        // salaries; only the ones that resolve to players belong in a snapshot
        let salaried_players: HashMap<SlateId, SalariedPlayersBySlot> =
            salaried_players::build_salaried_players(nfl, all_slates.as_slice())
                .await?
                .into_iter()
                .filter(|(_, by_slot)| !by_slot.is_empty())
                .collect();

        let slates = all_slates
            .into_iter()
            .filter(|slate| salaried_players.contains_key(&slate.id))
            .map(|slate| (slate.id, slate))
            .collect();

        Ok(Self {
            slates,
            salaried_players,
        })
    }

    /// All slates with salaried players in ascending order
    pub fn slates(&self) -> impl Iterator<Item = &Slate> + '_ {
        self.slates.values()
    }

    pub fn salaried_players_for_position(
        &self,
        slate_id: &SlateId,
        pos: &DfsPositionSlotType,
    ) -> &[SalariedPlayer] {
        self.salaried_players
            .get(slate_id)
            .and_then(|by_slot| by_slot.get(pos))
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }
}
