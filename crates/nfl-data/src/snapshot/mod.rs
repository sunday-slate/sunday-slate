use crate::salaried_players::{self, SalariedPlayer, SalariedPlayersBySlot};
use crate::slates::{self, Slate, SlateId};
use crate::store::player_link;
use crate::{NflData, NflDataError};
use nfl_model::{
    DfsPositionSlotType, Game, InjuryDesignation, NflPlayerId, PlayerOrDefenseId, TeamAbbr,
};
use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, HashMap, HashSet};

#[cfg(test)]
mod tests;

/// One slate's salaried players: the position buckets plus direct lookups by
/// gsis id and by team.
#[derive(Debug, Default)]
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

async fn annotate_injuries(
    nfl: &NflData,
    built: &mut HashMap<SlateId, SalariedPlayersBySlot>,
    current_week_slate_ids: &[SlateId],
) -> Result<(), NflDataError> {
    let Some(report) = nfl
        .injury_report()
        .filter(|report| !report.entries.is_empty())
    else {
        return Ok(());
    };
    let eligible_rows = current_week_slate_ids.iter().any(|slate_id| {
        built
            .get(slate_id)
            .is_some_and(|by_slot| by_slot.values().flatten().any(|row| row.id.is_some()))
    });
    if !eligible_rows {
        return Ok(());
    }

    let links_by_espn = player_link::links_by_espn(nfl.store.reader()).await?;
    let mut designations: HashMap<NflPlayerId, Option<InjuryDesignation>> = HashMap::new();
    for entry in &report.entries {
        let Some(espn_id) = entry
            .espn_id
            .as_ref()
            .map(|espn| espn.0.trim())
            .filter(|espn| !espn.is_empty())
        else {
            continue;
        };
        let Some(gsis_id) = links_by_espn.get(espn_id) else {
            continue;
        };
        match designations.entry(NflPlayerId(gsis_id.clone())) {
            Entry::Vacant(slot) => {
                slot.insert(Some(entry.designation));
            }
            Entry::Occupied(mut slot) => {
                if *slot.get() != Some(entry.designation) {
                    slot.insert(None);
                }
            }
        }
    }
    if designations.is_empty() {
        return Ok(());
    }

    for slate_id in current_week_slate_ids {
        if let Some(by_slot) = built.get_mut(slate_id) {
            for row in by_slot.values_mut().flatten() {
                if let Some(id) = &row.id {
                    row.injury = designations.get(id).copied().flatten();
                }
            }
        }
    }
    Ok(())
}

// Input is ordered by SlateId.
fn select_current_slate_id<'a>(
    slates: impl Iterator<Item = &'a Slate>,
    now: time::OffsetDateTime,
) -> Option<SlateId> {
    let mut last_known = None;
    let mut greatest = None;
    for slate in slates {
        greatest = Some(slate.id);
        if slate.kickoff.is_none() {
            continue;
        }
        last_known = Some(slate.id);
        if !slate.finished(now) {
            return Some(slate.id);
        }
    }
    last_known.or(greatest)
}

#[derive(Debug, Default)]
pub struct Snapshot {
    slates: BTreeMap<SlateId, Slate>,
    current_week_slate_ids: Vec<SlateId>,
    current_slate_id: Option<SlateId>,
    games: HashMap<SlateId, Vec<Game>>,
    salaried_players: HashMap<SlateId, SalariedPlayers>,
    stats_available: HashSet<SlateId>,
}

impl Snapshot {
    /// Build from the handle: the configured season's stored slates, then the
    /// salaried players that resolve for them.
    pub(crate) async fn build(nfl: &NflData) -> Result<Self, NflDataError> {
        Self::build_at(nfl, time::OffsetDateTime::now_utc()).await
    }

    async fn build_at(nfl: &NflData, now: time::OffsetDateTime) -> Result<Self, NflDataError> {
        let all_slates: Vec<Slate> = slates::all(nfl).await?.into_iter().collect();

        // Every stored slate's games, resolved at build time; independent of
        // whether the slate resolves to salaried players.
        let mut games: HashMap<SlateId, Vec<Game>> = HashMap::with_capacity(all_slates.len());
        for slate in &all_slates {
            games.insert(slate.id, slates::games(nfl, &slate.id).await?);
        }

        let (mut built, available) =
            salaried_players::build_salaried_players(nfl, all_slates.as_slice(), &games).await?;
        built.retain(|_, by_slot| !by_slot.is_empty());

        let slates: BTreeMap<SlateId, Slate> = all_slates
            .into_iter()
            .filter(|slate| built.contains_key(&slate.id))
            .map(|slate| (slate.id, slate))
            .collect();

        // SlateId groups slates by week.
        let current_week_slate_ids: Vec<SlateId> = match slates.last_key_value() {
            Some((_, latest)) => {
                let mut ids: Vec<SlateId> = slates
                    .values()
                    .rev()
                    .take_while(|slate| slate.season_week == latest.season_week)
                    .map(|slate| slate.id)
                    .collect();
                ids.reverse();
                ids
            }
            None => Vec::new(),
        };

        annotate_injuries(nfl, &mut built, &current_week_slate_ids).await?;

        let salaried: HashMap<SlateId, SalariedPlayers> = built
            .into_iter()
            .map(|(slate_id, by_slot)| (slate_id, SalariedPlayers::new(by_slot)))
            .collect();

        let stats_available = available
            .into_iter()
            .filter(|slate_id| slates.contains_key(slate_id))
            .collect();

        let current_slate_id = select_current_slate_id(
            current_week_slate_ids
                .iter()
                .filter_map(|slate_id| slates.get(slate_id)),
            now,
        );

        Ok(Self {
            slates,
            current_week_slate_ids,
            current_slate_id,
            games,
            salaried_players: salaried,
            stats_available,
        })
    }

    /// Complete team-game coverage in this publication; not a game-finality guarantee.
    pub fn stats_available(&self, slate_id: &SlateId) -> bool {
        self.stats_available.contains(slate_id)
    }

    /// All slates with salaried players in ascending order
    pub fn slates(&self) -> impl Iterator<Item = &Slate> + '_ {
        self.slates.values()
    }

    /// Latest populated week's slates in ID order. Only these carry injuries.
    pub fn current_week_slates(&self) -> impl Iterator<Item = &Slate> + '_ {
        self.current_week_slate_ids
            .iter()
            .filter_map(|slate_id| self.slates.get(slate_id))
    }

    /// Current slate, fixed at build time.
    pub fn current_slate(&self) -> Option<&Slate> {
        self.current_slate_id
            .as_ref()
            .and_then(|slate_id| self.slates.get(slate_id))
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
