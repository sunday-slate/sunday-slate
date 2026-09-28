use crate::contests::{Contest, ContestId};
use crate::fantasy_teams::FantasyTeamId;
use crate::media::Media;
use nfl_data::TeamAbbr as NflTeamAbbr;

/// A typed NFL-player reference. Wraps nfl-data's `String` gsis ids at the
/// boundary; its eventual home is nfl-data next to `NflTeamAbbr`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, sqlx::Type)]
#[sqlx(transparent)]
pub struct NflPlayerId(pub String);

impl AsRef<str> for NflPlayerId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// A lineup's nine roster slots. Stored as TEXT (`"QB"`, `"RB1"`, ...) in
/// `entry_slots.roster_slot`; the sqlx derive decodes that column directly.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    sqlx::Type,
    strum::Display,
    strum::EnumString,
    strum::EnumIter,
)]
#[sqlx(rename_all = "UPPERCASE")]
#[strum(serialize_all = "UPPERCASE")]
pub enum RosterSlot {
    Qb,
    Rb1,
    Rb2,
    Wr1,
    Wr2,
    Wr3,
    Te,
    Flex,
    Def,
}

impl RosterSlot {
    /// The position a slot draws from, without its ordinal: `Rb1` and `Rb2`
    /// are both `"RB"`. This is the display label the entry page shows, kept
    /// next to the slot so a new variant cannot drift from its label.
    pub fn position_label(self) -> &'static str {
        use RosterSlot::*;
        match self {
            Qb => "QB",
            Rb1 | Rb2 => "RB",
            Wr1 | Wr2 | Wr3 => "WR",
            Te => "TE",
            Flex => "FLEX",
            Def => "DEF",
        }
    }

    pub fn dom_id(self) -> &'static str {
        match self {
            Self::Qb => "qb",
            Self::Rb1 => "rb1",
            Self::Rb2 => "rb2",
            Self::Wr1 => "wr1",
            Self::Wr2 => "wr2",
            Self::Wr3 => "wr3",
            Self::Te => "te",
            Self::Flex => "flex",
            Self::Def => "def",
        }
    }
}

/// What a roster slot holds: a player for the eight player slots, a team
/// defense for DEF.
#[derive(Debug, Clone, PartialEq)]
pub enum SlotValue {
    Player(NflPlayerId),
    Defense(NflTeamAbbr),
}

/// One `entry_slots` row, typed: which slot and what it holds.
#[derive(Debug, Clone, PartialEq)]
pub struct EntrySlot {
    pub slot: RosterSlot,
    pub value: SlotValue,
}

/// The one lineup invariant the schema can't enforce: all nine slots present.
/// (Valid slot names, payload shape, and per-entry uniqueness are CHECK/UNIQUE
/// constraints on `entry_slots`.)
#[derive(Debug, PartialEq, thiserror::Error)]
pub enum LineupError {
    #[error("missing {0} slot")]
    MissingSlot(RosterSlot),
}

/// Total — an entry exists in a contest, so all nine slots are filled.
#[derive(Debug, Clone, PartialEq)]
pub struct Lineup {
    pub qb: NflPlayerId,
    pub rb1: NflPlayerId,
    pub rb2: NflPlayerId,
    pub wr1: NflPlayerId,
    pub wr2: NflPlayerId,
    pub wr3: NflPlayerId,
    pub te: NflPlayerId,
    pub flex: NflPlayerId,
    pub def: NflTeamAbbr,
}

impl Lineup {
    /// The eight player slots in roster order, each paired with the slot it
    /// fills. Kept next to the fields so a caller that walks the lineup
    /// cannot miss a slot.
    pub fn player_slots(&self) -> [(RosterSlot, &NflPlayerId); 8] {
        use RosterSlot::*;
        [
            (Qb, &self.qb),
            (Rb1, &self.rb1),
            (Rb2, &self.rb2),
            (Wr1, &self.wr1),
            (Wr2, &self.wr2),
            (Wr3, &self.wr3),
            (Te, &self.te),
            (Flex, &self.flex),
        ]
    }

    /// The eight player ids in roster order.
    pub fn player_ids(&self) -> [&NflPlayerId; 8] {
        self.player_slots().map(|(_, id)| id)
    }

    /// The nine `EntrySlot`s the lineup maps to, in roster order — the
    /// inverse of [`Self::from_slots`].
    pub(crate) fn to_slots(&self) -> Vec<EntrySlot> {
        let mut slots: Vec<EntrySlot> = self
            .player_slots()
            .into_iter()
            .map(|(slot, id)| EntrySlot {
                slot,
                value: SlotValue::Player(id.clone()),
            })
            .collect();
        slots.push(EntrySlot {
            slot: RosterSlot::Def,
            value: SlotValue::Defense(self.def.clone()),
        });
        slots
    }

    /// Assemble one entry's slots into a total lineup, reporting the first
    /// hole. A slot whose value kind doesn't match (impossible for rows read
    /// through the schema) fills nothing and so surfaces as that slot missing.
    pub(crate) fn from_slots(
        slots: impl IntoIterator<Item = EntrySlot>,
    ) -> Result<Lineup, LineupError> {
        use RosterSlot::*;

        #[derive(Default)]
        struct Builder {
            qb: Option<NflPlayerId>,
            rb1: Option<NflPlayerId>,
            rb2: Option<NflPlayerId>,
            wr1: Option<NflPlayerId>,
            wr2: Option<NflPlayerId>,
            wr3: Option<NflPlayerId>,
            te: Option<NflPlayerId>,
            flex: Option<NflPlayerId>,
            def: Option<NflTeamAbbr>,
        }

        let mut b = Builder::default();
        for EntrySlot { slot, value } in slots {
            match (slot, value) {
                (Qb, SlotValue::Player(p)) => b.qb = Some(p),
                (Rb1, SlotValue::Player(p)) => b.rb1 = Some(p),
                (Rb2, SlotValue::Player(p)) => b.rb2 = Some(p),
                (Wr1, SlotValue::Player(p)) => b.wr1 = Some(p),
                (Wr2, SlotValue::Player(p)) => b.wr2 = Some(p),
                (Wr3, SlotValue::Player(p)) => b.wr3 = Some(p),
                (Te, SlotValue::Player(p)) => b.te = Some(p),
                (Flex, SlotValue::Player(p)) => b.flex = Some(p),
                (Def, SlotValue::Defense(t)) => b.def = Some(t),
                _ => {}
            }
        }
        let take = |v: Option<NflPlayerId>, slot| v.ok_or(LineupError::MissingSlot(slot));
        Ok(Lineup {
            qb: take(b.qb, Qb)?,
            rb1: take(b.rb1, Rb1)?,
            rb2: take(b.rb2, Rb2)?,
            wr1: take(b.wr1, Wr1)?,
            wr2: take(b.wr2, Wr2)?,
            wr3: take(b.wr3, Wr3)?,
            te: take(b.te, Te)?,
            flex: take(b.flex, Flex)?,
            def: b.def.ok_or(LineupError::MissingSlot(Def))?,
        })
    }
}

/// An entry seen from its contest's side: the contest is context, the fantasy
/// team is data — the symmetric view to [`FantasyTeamEntry`].
#[derive(Debug, Clone, PartialEq)]
pub struct ContestEntry {
    /// The `/entries/{id}` link target.
    pub id: EntryId,
    pub fantasy_team: FantasyTeamId,
    pub team_name: String,
    pub owner_name: String,
    pub logo: Option<Media>,
    pub lineup: Lineup,
}

/// An entry seen from its fantasy team's side: the fantasy team is context
/// (the owning [`FantasyTeamEntries`]), the contest is data. The contest-major
/// view is the symmetric [`ContestEntry`]; a player-picks view gets a struct
/// carrying both ids.
#[derive(Debug, Clone, PartialEq)]
pub struct FantasyTeamEntry {
    pub contest: ContestId,
    pub lineup: Lineup,
}

/// Aggregate root: a fantasy team and its contest entries. Ownership is the
/// fantasy team↔entry relationship.
#[derive(Debug, Clone, PartialEq)]
pub struct FantasyTeamEntries {
    pub id: FantasyTeamId,
    pub name: String,
    pub owner_name: String,
    pub logo: Option<Media>,
    pub entries: Vec<FantasyTeamEntry>,
}

/// An entry's id as an opaque token — the `/entries/{id}` route key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, sqlx::Type)]
#[sqlx(transparent)]
pub struct EntryId(pub i64);

/// One entry loaded for display: its lineup plus the context the page needs —
/// contest, league, and owning fantasy team.
#[derive(Debug, Clone)]
pub struct EntryDetails {
    pub id: EntryId,
    pub league_id: i64,
    pub contest: Contest,
    pub team: EntryTeam,
    pub lineup: Lineup,
}

/// The owning fantasy team, as the entry page needs it.
#[derive(Debug, Clone)]
pub struct EntryTeam {
    pub id: FantasyTeamId,
    /// Who owns the entry — the page shows unplayed picks to this user alone.
    pub user_id: i64,
    pub name: String,
    pub owner_name: String,
    pub logo: Option<Media>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;
    use strum::IntoEnumIterator;

    fn player(slot: RosterSlot, gsis: &str) -> EntrySlot {
        EntrySlot {
            slot,
            value: SlotValue::Player(NflPlayerId(gsis.to_string())),
        }
    }

    fn full_slots() -> Vec<EntrySlot> {
        use RosterSlot::*;
        vec![
            player(Qb, "00-1"),
            player(Rb1, "00-2"),
            player(Rb2, "00-3"),
            player(Wr1, "00-4"),
            player(Wr2, "00-5"),
            player(Wr3, "00-6"),
            player(Te, "00-7"),
            player(Flex, "00-8"),
            EntrySlot {
                slot: Def,
                value: SlotValue::Defense(NflTeamAbbr("KC".to_string())),
            },
        ]
    }

    #[test]
    fn roster_slot_strings_round_trip_the_db_forms() {
        // The DB CHECK constraint speaks these exact strings; pin them.
        let names: Vec<String> = RosterSlot::iter().map(|s| s.to_string()).collect();
        assert_eq!(
            names,
            ["QB", "RB1", "RB2", "WR1", "WR2", "WR3", "TE", "FLEX", "DEF"]
        );
        for slot in RosterSlot::iter() {
            assert_eq!(RosterSlot::from_str(&slot.to_string()), Ok(slot));
        }
    }

    #[test]
    fn position_label_drops_the_slot_ordinal() {
        let labels: Vec<&str> = RosterSlot::iter().map(|s| s.position_label()).collect();
        assert_eq!(
            labels,
            ["QB", "RB", "RB", "WR", "WR", "WR", "TE", "FLEX", "DEF"]
        );
    }

    #[test]
    fn player_slots_covers_every_slot_but_def_in_roster_order() {
        let lineup = Lineup::from_slots(full_slots()).expect("nine valid slots");
        let slots: Vec<RosterSlot> = lineup.player_slots().map(|(s, _)| s).into();
        let expected: Vec<RosterSlot> = RosterSlot::iter()
            .filter(|s| *s != RosterSlot::Def)
            .collect();
        assert_eq!(slots, expected);
        let ids: Vec<&NflPlayerId> = lineup.player_ids().into();
        assert_eq!(ids.first(), Some(&&NflPlayerId("00-1".into())));
        assert_eq!(ids.last(), Some(&&NflPlayerId("00-8".into())));
    }

    #[test]
    fn from_slots_builds_a_total_lineup() {
        let lineup = Lineup::from_slots(full_slots()).expect("nine valid slots");
        assert_eq!(lineup.qb, NflPlayerId("00-1".into()));
        assert_eq!(lineup.flex, NflPlayerId("00-8".into()));
        assert_eq!(lineup.def, NflTeamAbbr("KC".into()));
    }

    #[test]
    fn from_slots_rejects_missing_slot() {
        let mut slots = full_slots();
        slots.pop(); // drop DEF
        let err = Lineup::from_slots(slots).unwrap_err();
        assert_eq!(err, LineupError::MissingSlot(RosterSlot::Def));
    }

    #[test]
    fn from_slots_reports_mismatched_value_as_missing() {
        // DEF carrying a player can't come from the DB (CHECK constraint);
        // if constructed anyway it fills nothing, so DEF reads as missing.
        let mut slots = full_slots();
        slots[8] = player(RosterSlot::Def, "00-9");
        let err = Lineup::from_slots(slots).unwrap_err();
        assert_eq!(err, LineupError::MissingSlot(RosterSlot::Def));
    }
}
