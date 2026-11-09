use crate::salaried_players::{self, SalariedPlayer, SalariedPlayersBySlot};
use crate::slates::{self, Slate, SlateId};
use crate::{NflData, NflDataError};
use nfl_model::{DfsPositionSlotType, Game, NflPlayerId, PlayerOrDefenseId, TeamAbbr};
use std::collections::{BTreeMap, HashMap};

#[cfg(test)]
mod tests;

/// One slate's salaried players: the position buckets plus direct lookups by
/// gsis id and by team.
#[derive(Default)]
struct SalariedPlayers {
    by_slot: SalariedPlayersBySlot,
    players: HashMap<NflPlayerId, SalariedPlayer>,
    defenses: HashMap<TeamAbbr, SalariedPlayer>,
}

impl SalariedPlayers {
    /// Index the position buckets. Every row lands in exactly one `Position`
    /// bucket, so the flex duplicates are skipped.
    fn new(by_slot: SalariedPlayersBySlot) -> Self {
        let mut players = HashMap::new();
        let mut defenses = HashMap::new();
        for (slot_type, rows) in &by_slot {
            if matches!(slot_type, DfsPositionSlotType::Flex) {
                continue;
            }
            for row in rows {
                match &row.id {
                    Some(id) => {
                        players.insert(id.clone(), row.clone());
                    }
                    None => {
                        defenses.insert(row.team, row.clone());
                    }
                }
            }
        }
        Self {
            by_slot,
            players,
            defenses,
        }
    }
}

#[derive(Default)]
pub struct Snapshot {
    slates: BTreeMap<SlateId, Slate>,
    games: HashMap<SlateId, Vec<Game>>,
    salaried_players: HashMap<SlateId, SalariedPlayers>,
}

impl Snapshot {
    /// Build from the handle: the configured season's stored slates, then the
    /// salaried players that resolve for them.
    pub(crate) async fn build(nfl: &NflData) -> Result<Self, NflDataError> {
        let all_slates: Vec<Slate> = slates::all(nfl).await?.into_iter().collect();

        // Every stored slate's games, resolved at build time; independent of
        // whether the slate resolves to salaried players.
        let mut games: HashMap<SlateId, Vec<Game>> = HashMap::with_capacity(all_slates.len());
        for slate in &all_slates {
            games.insert(slate.id, slates::games(nfl, &slate.id).await?);
        }

        // stored slates exist for every week, even those without games or
        // salaries; only the ones that resolve to players belong in a snapshot
        let salaried: HashMap<SlateId, SalariedPlayers> =
            salaried_players::build_salaried_players(nfl, all_slates.as_slice())
                .await?
                .into_iter()
                .filter(|(_, by_slot)| !by_slot.is_empty())
                .map(|(slate_id, by_slot)| (slate_id, SalariedPlayers::new(by_slot)))
                .collect();

        let slates = all_slates
            .into_iter()
            .filter(|slate| salaried.contains_key(&slate.id))
            .map(|slate| (slate.id, slate))
            .collect();

        Ok(Self {
            slates,
            games,
            salaried_players: salaried,
        })
    }

    /// All slates with salaried players in ascending order
    pub fn slates(&self) -> impl Iterator<Item = &Slate> + '_ {
        self.slates.values()
    }

    /// Greatest populated slate id; `None` when empty. ID order, not kickoff.
    pub fn current_slate(&self) -> Option<&Slate> {
        self.slates.last_key_value().map(|(_, slate)| slate)
    }

    /// The slate with this id, or `None` when it has no salaried players.
    pub fn slate(&self, id: &SlateId) -> Option<&Slate> {
        self.slates.get(id)
    }

    /// The stored slate's games as captured at build time, independent of
    /// whether the slate resolves to salaried players: known kickoffs
    /// ascending, unknown kickoffs last, game id breaking ties. `None` for the
    /// slate itself does not make this empty. Unknown ids and slates with no
    /// membership yield an empty slice.
    pub fn games_for_slate(&self, slate_id: &SlateId) -> &[Game] {
        self.games.get(slate_id).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Player-only index for `slate_id`; `None` when unpopulated.
    /// All positions, no flex duplicates, no defenses.
    pub fn players_for_slate(
        &self,
        slate_id: &SlateId,
    ) -> Option<&HashMap<NflPlayerId, SalariedPlayer>> {
        self.salaried_players
            .get(slate_id)
            .map(|slate| &slate.players)
    }

    /// The player with this gsis id on `slate_id`, or `None` when the slate
    /// has no such player.
    pub fn player(&self, slate_id: &SlateId, id: &NflPlayerId) -> Option<&SalariedPlayer> {
        self.salaried_players.get(slate_id)?.players.get(id)
    }

    /// The defense with this team on `slate_id`, or `None` when the slate has
    /// no such defense.
    pub fn defense(&self, slate_id: &SlateId, team: &TeamAbbr) -> Option<&SalariedPlayer> {
        self.salaried_players.get(slate_id)?.defenses.get(team)
    }

    /// The salaried row for a player or a defense, whichever the id names.
    pub fn salaried_player(
        &self,
        slate_id: &SlateId,
        value: &PlayerOrDefenseId,
    ) -> Option<&SalariedPlayer> {
        match value {
            PlayerOrDefenseId::Player(id) => self.player(slate_id, id),
            PlayerOrDefenseId::Defense(team) => self.defense(slate_id, team),
        }
    }

    pub fn salaried_players_for_position(
        &self,
        slate_id: &SlateId,
        pos: &DfsPositionSlotType,
    ) -> &[SalariedPlayer] {
        self.salaried_players
            .get(slate_id)
            .and_then(|slate| slate.by_slot.get(pos))
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }
}
