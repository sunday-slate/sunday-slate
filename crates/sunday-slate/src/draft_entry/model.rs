use nfl_data::TeamAbbr as NflTeamAbbr;
use strum::IntoEnumIterator;

use crate::contests::ContestId;
use crate::entries::{NflPlayerId, RosterSlot, SlotValue};
use crate::fantasy_teams::FantasyTeamId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, sqlx::Type)]
#[sqlx(transparent)]
pub struct DraftEntryId(pub i64);

/// A draft lineup: one optional pick per roster slot. Nulls are empty slots.
#[derive(Debug, Clone, PartialEq)]
pub struct DraftEntry {
    pub id: DraftEntryId,
    pub contest_id: ContestId,
    pub fantasy_team_id: FantasyTeamId,
    pub qb: Option<NflPlayerId>,
    pub rb1: Option<NflPlayerId>,
    pub rb2: Option<NflPlayerId>,
    pub wr1: Option<NflPlayerId>,
    pub wr2: Option<NflPlayerId>,
    pub wr3: Option<NflPlayerId>,
    pub te: Option<NflPlayerId>,
    pub flex: Option<NflPlayerId>,
    pub def: Option<NflTeamAbbr>,
}

impl DraftEntry {
    /// The value in a slot, `None` for an empty one.
    pub fn get(&self, slot: RosterSlot) -> Option<SlotValue> {
        let p = |id: &Option<NflPlayerId>| id.clone().map(SlotValue::Player);
        match slot {
            RosterSlot::Qb => p(&self.qb),
            RosterSlot::Rb1 => p(&self.rb1),
            RosterSlot::Rb2 => p(&self.rb2),
            RosterSlot::Wr1 => p(&self.wr1),
            RosterSlot::Wr2 => p(&self.wr2),
            RosterSlot::Wr3 => p(&self.wr3),
            RosterSlot::Te => p(&self.te),
            RosterSlot::Flex => p(&self.flex),
            RosterSlot::Def => self.def.clone().map(SlotValue::Defense),
        }
    }

    /// Put a value in a slot, overwriting whatever was there.
    pub fn set(&mut self, slot: RosterSlot, value: SlotValue) {
        let (player, defense) = match value {
            SlotValue::Player(id) => (Some(id), None),
            SlotValue::Defense(t) => (None, Some(t)),
        };
        match slot {
            RosterSlot::Qb => self.qb = player,
            RosterSlot::Rb1 => self.rb1 = player,
            RosterSlot::Rb2 => self.rb2 = player,
            RosterSlot::Wr1 => self.wr1 = player,
            RosterSlot::Wr2 => self.wr2 = player,
            RosterSlot::Wr3 => self.wr3 = player,
            RosterSlot::Te => self.te = player,
            RosterSlot::Flex => self.flex = player,
            RosterSlot::Def => self.def = defense,
        }
    }

    /// Empty a slot, discarding whatever pick it held.
    pub fn clear(&mut self, slot: RosterSlot) {
        match slot {
            RosterSlot::Qb => self.qb = None,
            RosterSlot::Rb1 => self.rb1 = None,
            RosterSlot::Rb2 => self.rb2 = None,
            RosterSlot::Wr1 => self.wr1 = None,
            RosterSlot::Wr2 => self.wr2 = None,
            RosterSlot::Wr3 => self.wr3 = None,
            RosterSlot::Te => self.te = None,
            RosterSlot::Flex => self.flex = None,
            RosterSlot::Def => self.def = None,
        }
    }

    /// Whether any slot holds a pick — a draft worth resuming.
    pub fn has_picks(&self) -> bool {
        RosterSlot::iter().any(|s| self.get(s).is_some())
    }

    /// The gsis ids currently occupying the eight player slots.
    pub fn filled_player_ids(&self) -> Vec<NflPlayerId> {
        [
            &self.qb, &self.rb1, &self.rb2, &self.wr1, &self.wr2, &self.wr3, &self.te, &self.flex,
        ]
        .into_iter()
        .flatten()
        .cloned()
        .collect()
    }
}
