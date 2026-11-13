//! The draft-entry editor's service layer: cap math, candidate pools, and the
//! page / slot / save operations the handler and templates consume.

use std::collections::HashSet;
use std::sync::Arc;

use nfl_data::DfsPosition;
use nfl_data::salaried_players::SalariedPlayer;
use nfl_data::slates::{Slate, SlateId};
use nfl_data::snapshot::Snapshot;
use nfl_model::DfsPositionSlotType;
use strum::IntoEnumIterator;
use time::OffsetDateTime;

use crate::draft_entry::model::DraftEntry;
use crate::draft_entry::store as draft_store;
use crate::entries::service::{self as entries_service, PickInfo, SALARY_CAP};
use crate::entries::store as entries_store;
use crate::entries::{EntryId, EntrySlot, Lineup, PlayerOrDefenseId, RosterSlot};
use crate::fantasy_teams::FantasyTeamId;
use crate::injuries::InjuryChip;
use crate::media::Media;
use crate::scoring::format_thousands;
use crate::{AppError, AppState};

/// Remaining budget and the per-open-slot average. Negative remaining = over
/// the cap.
pub fn remaining_and_per_open(spent: i64, filled: u8) -> (i64, i64) {
    let remaining = SALARY_CAP - spent;
    let open = 9 - filled;
    let per_open = if open == 0 {
        0
    } else {
        remaining / open as i64
    };
    (remaining, per_open)
}

/// The editor page: cap state, the nine rows, and whether the slate is read-only.
pub struct EditorPage {
    pub contest_name: String,
    pub team_name: String,
    pub cap: CapState,
    pub slots: Vec<EditorSlotRow>,
    pub locked: bool,
    /// The committed entry, when one exists: the first save creates it, later
    /// saves update it.
    pub entry_id: Option<EntryId>,
}

impl EditorPage {
    fn has_entry(&self) -> bool {
        self.entry_id.is_some()
    }

    /// The save gate: every slot filled and the lineup under the cap.
    pub fn can_save(&self) -> bool {
        self.cap.can_save()
    }

    /// The commit action: `"Save Entry"` first, `"Update Entry"` after.
    pub fn save_label(&self) -> &'static str {
        if self.has_entry() {
            "Update Entry"
        } else {
            "Save Entry"
        }
    }

    /// Abandoning the draft only loses unsaved work; the committed entry, if
    /// any, stays.
    pub fn discard_label(&self) -> &'static str {
        if self.has_entry() {
            "Discard Changes"
        } else {
            "Discard Draft"
        }
    }

    pub fn over_cap(&self) -> bool {
        self.cap.over_cap()
    }

    /// The band's headline amount: what's left under the cap, or the
    /// overage (unsigned — the "over the salary cap" label carries the
    /// sign).
    pub fn remaining_str(&self) -> String {
        format!("${}", format_thousands(self.cap.remaining.abs()))
    }

    /// The band's right-hand note: the per-open budget while slots stay
    /// open. `None` over the cap — the red headline already says it all.
    pub fn cap_note(&self) -> Option<String> {
        if self.over_cap() {
            None
        } else if self.cap.filled == 9 {
            Some("All positions filled".to_string())
        } else {
            Some(format!("${} per open", format_thousands(self.cap.per_open)))
        }
    }

    /// The open-slot count under the cap note, `None` once all are filled
    /// (or the lineup is locked and no longer editable).
    pub fn save_hint(&self) -> Option<String> {
        if self.locked {
            return None;
        }
        match 9 - self.cap.filled {
            0 => None,
            1 => Some("1 position left to fill".to_string()),
            open => Some(format!("{open} positions left to fill")),
        }
    }
}

/// One of the editor's nine slot rows: what fills the slot and how it prices.
pub struct EditorSlotRow {
    pub slot: RosterSlot,
    pub info: Option<PickInfo>,
}

impl EditorSlotRow {
    pub fn injury(&self) -> Option<InjuryChip> {
        self.info.as_ref().and_then(PickInfo::injury)
    }

    /// The row's avatar: the pick's, or the silhouette for an open slot.
    pub fn avatar_url(&self) -> String {
        self.info
            .as_ref()
            .map_or_else(entries_service::silhouette_url, PickInfo::avatar_url)
    }
}

/// One candidate for a slot's pick list, with its price and pace.
pub struct SlotChoice {
    pub info: PickInfo,
    pub is_picked: bool,
    pub over_cap: bool,
}

/// Open (or resume) the draft and assemble the editor page. `NotFound` when
/// the slate is not published with salaried players.
pub async fn editor_page(
    state: &AppState,
    team_id: FantasyTeamId,
    team_name: String,
    slate_id: SlateId,
) -> Result<EditorPage, AppError> {
    let reader = state.db.reader();

    let ctx = slate_ctx(state, slate_id).await?;

    // Resume an in-progress draft; seed a first open from the committed entry.
    let committed = entries_store::lineup_for_team(reader, slate_id, team_id).await?;
    let entry_id = committed.as_ref().map(|(id, _)| *id);
    let current_lineup = committed.and_then(|(_, lineup)| lineup);
    let draft_entry =
        get_or_create_draft(state, slate_id, team_id, current_lineup.as_ref()).await?;

    let player_dst_ids: Vec<PlayerOrDefenseId> = RosterSlot::iter()
        .filter_map(|slot| draft_entry.get(slot))
        .collect();
    let avatars = crate::avatars::by_id(&state.db, &player_dst_ids).await?;

    let roster_slots: Vec<EditorSlotRow> = RosterSlot::iter()
        .map(|roster_slot| {
            let player_dst_id = draft_entry.get(roster_slot);
            let player = player_dst_id
                .as_ref()
                .and_then(|pd_id| ctx.salaried_player(pd_id));
            let avatar = player_dst_id.as_ref().and_then(|pd_id| avatars.get(pd_id));
            build_slot_row(roster_slot, player, avatar)
        })
        .collect();

    let cap = CapState::new(&draft_entry, &ctx);

    Ok(EditorPage {
        contest_name: ctx.slate.name.clone(),
        team_name,
        cap,
        slots: roster_slots,
        locked: ctx.locked(),
        entry_id,
    })
}

/// The candidate list for one slot, salary-descending from the snapshot.
/// Omits players already picked into another slot; the current occupant of
/// `slot` appears marked `is_picked`. Defense choices carry the team abbr as
/// the value key.
pub async fn choices(
    state: &AppState,
    team_id: FantasyTeamId,
    slate_id: SlateId,
    slot: RosterSlot,
) -> Result<Vec<SlotChoice>, AppError> {
    // Resolve the slate first: a contest with no playable slate is `NotFound`,
    // and must not leave a draft row behind.
    let ctx = slate_ctx(state, slate_id).await?;
    let draft = get_or_create_draft(state, slate_id, team_id, None).await?;

    // The budget this slot may spend: the cap minus what the *other* slots
    // hold. Swapping replaces the current occupant, so it doesn't count.
    let spent_elsewhere: i64 = RosterSlot::iter()
        .filter(|s| *s != slot)
        .filter_map(|s| slot_salary(&draft, s, &ctx))
        .sum();
    let budget = SALARY_CAP - spent_elsewhere;

    let picked_elsewhere: HashSet<String> = RosterSlot::iter()
        .filter(|s| *s != slot)
        .filter_map(|s| match draft.get(s) {
            Some(PlayerOrDefenseId::Player(id)) => Some(id.0),
            _ => None,
        })
        .collect();

    let visible: Vec<&SalariedPlayer> = ctx
        .snapshot
        .salaried_players_for_position(&ctx.slate.id, &slot_type(slot))
        .iter()
        .filter(|player| {
            player
                .id
                .as_ref()
                .is_none_or(|id| !picked_elsewhere.contains(id.as_ref()))
        })
        .collect();

    let values: Vec<PlayerOrDefenseId> = visible.iter().map(|player| value_of(player)).collect();
    let avatars = crate::avatars::by_id(&state.db, &values).await?;

    let same_player = match draft.get(slot) {
        Some(PlayerOrDefenseId::Player(id)) => Some(id),
        _ => None,
    };
    let same_defense = draft.def;

    Ok(visible
        .into_iter()
        .map(|player| {
            let value = value_of(player);
            let media = avatars.get(&value);
            let is_picked = match player.id.as_ref() {
                Some(id) => same_player.as_ref() == Some(id),
                None => same_defense == Some(player.team),
            };
            SlotChoice {
                info: PickInfo::new(player.clone(), media),
                is_picked,
                over_cap: i64::from(player.salary) > budget,
            }
        })
        .collect())
}

/// Place (or replace) a pick in a slot. Rejects an invalid pick (see
/// [`validate_pick`]) and refuses on a locked slate.
pub async fn set_slot(
    state: &AppState,
    team_id: FantasyTeamId,
    slate_id: SlateId,
    slot: RosterSlot,
    value: PlayerOrDefenseId,
) -> Result<(), AppError> {
    let ctx = require_open(state, slate_id).await?;
    mutate_draft(state, slate_id, team_id, move |draft| {
        validate_pick(draft, slot, &value, &ctx)?;
        draft.set(slot, value);
        Ok(())
    })
    .await
}

/// Empty a slot. An absent draft is created then cleared (net no-op).
/// Refuses on a locked slate.
pub async fn clear_slot(
    state: &AppState,
    team_id: FantasyTeamId,
    slate_id: SlateId,
    slot: RosterSlot,
) -> Result<(), AppError> {
    require_open(state, slate_id).await?;
    mutate_draft(state, slate_id, team_id, move |draft| {
        draft.clear(slot);
        Ok(())
    })
    .await
}

/// Load-or-create the draft, apply `mutate`, and persist — one transaction on
/// the write pool (the reader is read-only), so a rejected mutation also rolls
/// back an incidental draft creation.
async fn mutate_draft(
    state: &AppState,
    slate_id: SlateId,
    team_id: FantasyTeamId,
    mutate: impl FnOnce(&mut DraftEntry) -> Result<(), AppError> + Send,
) -> Result<(), AppError> {
    state
        .db
        .write_tx::<_, (), AppError>(async move |conn| {
            let mut draft = draft_store::get_or_create_tx(conn, slate_id, team_id, None).await?;
            mutate(&mut draft)?;
            draft_store::save(conn, &draft).await?;
            Ok(())
        })
        .await
}

/// Delete the draft row, abandoning the in-progress lineup. Never refused:
/// there is no committed data to protect.
pub async fn discard(
    state: &AppState,
    team_id: FantasyTeamId,
    slate_id: SlateId,
) -> Result<(), AppError> {
    state
        .db
        .write_tx::<_, (), AppError>(async move |conn| {
            draft_store::delete_by_keys(conn, slate_id, team_id).await?;
            Ok(())
        })
        .await
}

/// Commit the draft as a real entry: upsert the `entries` row, replace its
/// nine `entry_slots`, and delete the draft row. Refuses when the lineup is
/// incomplete or over the salary cap.
pub async fn save(
    state: &AppState,
    team_id: FantasyTeamId,
    slate_id: SlateId,
) -> Result<EntryId, AppError> {
    let ctx = require_open(state, slate_id).await?;
    state
        .db
        .write_tx::<_, EntryId, AppError>(async move |conn| {
            let Some(draft) = draft_store::by_keys(&mut *conn, slate_id, team_id).await? else {
                return Err(AppError::BadRequest(
                    "Complete all nine slots before saving.".into(),
                ));
            };
            let cap = CapState::new(&draft, &ctx);
            if !cap.can_save() {
                return Err(AppError::BadRequest(if cap.filled < 9 {
                    "Complete all nine slots before saving.".into()
                } else {
                    "Lineup is over the salary cap.".into()
                }));
            }
            let lineup = Lineup::from_slots(RosterSlot::iter().map(|s| EntrySlot {
                slot: s,
                value: draft.get(s).expect("can_save guarantees nine filled slots"),
            }))
            .map_err(|e| AppError::BadRequest(e.to_string()))?;
            let id = entries_store::upsert_with_lineup(conn, slate_id, team_id, &lineup).await?;
            draft_store::delete(&mut *conn, draft.id).await?;
            Ok(id)
        })
        .await
}

/// What a salaried row is, as a slot value.
fn value_of(player: &SalariedPlayer) -> PlayerOrDefenseId {
    match player.id.as_ref() {
        Some(id) => PlayerOrDefenseId::Player(id.clone()),
        None => PlayerOrDefenseId::Defense(player.team),
    }
}

/// The snapshot bucket a roster slot draws from.
fn slot_type(slot: RosterSlot) -> DfsPositionSlotType {
    match slot {
        RosterSlot::Qb => DfsPositionSlotType::Position(DfsPosition::Qb),
        RosterSlot::Rb1 | RosterSlot::Rb2 => DfsPositionSlotType::Position(DfsPosition::Rb),
        RosterSlot::Wr1 | RosterSlot::Wr2 | RosterSlot::Wr3 => {
            DfsPositionSlotType::Position(DfsPosition::Wr)
        }
        RosterSlot::Te => DfsPositionSlotType::Position(DfsPosition::Te),
        RosterSlot::Flex => DfsPositionSlotType::Flex,
        RosterSlot::Def => DfsPositionSlotType::Position(DfsPosition::Dst),
    }
}

/// The picker only offers valid choices, so these rejections are the
/// defense-in-depth path: a stale or tampered form fails with a message
/// instead of a constraint error.
fn validate_pick(
    draft: &DraftEntry,
    slot: RosterSlot,
    value: &PlayerOrDefenseId,
    ctx: &SlateCtx,
) -> Result<(), AppError> {
    match value {
        PlayerOrDefenseId::Defense(team) => {
            if slot != RosterSlot::Def {
                return Err(AppError::BadRequest(format!(
                    "{} is a defense; the {} slot takes a player.",
                    team.as_str(),
                    slot.position_label()
                )));
            }
            if ctx.salaried_player(value).is_none() {
                return Err(AppError::BadRequest(format!(
                    "{} is not on this slate.",
                    team.as_str()
                )));
            }
            Ok(())
        }
        PlayerOrDefenseId::Player(id) => {
            if slot == RosterSlot::Def {
                return Err(AppError::BadRequest(
                    "DEF takes a team defense, not a player.".into(),
                ));
            }
            let position = ctx
                .salaried_player(value)
                .map(|player| player.position)
                .ok_or_else(|| {
                    AppError::BadRequest(format!("{} is not on this slate.", id.as_ref()))
                })?;
            if !slot_type(slot).allows(position) {
                return Err(AppError::BadRequest(format!(
                    "{} does not fit the {} slot.",
                    id.as_ref(),
                    slot.position_label()
                )));
            }
            let elsewhere = RosterSlot::iter()
                .filter(|s| *s != slot)
                .filter_map(|s| match draft.get(s) {
                    Some(PlayerOrDefenseId::Player(p)) => Some(p),
                    _ => None,
                })
                .any(|p| p.as_ref() == id.as_ref());
            if elsewhere {
                return Err(AppError::BadRequest(format!(
                    "{} is already in this lineup.",
                    id.as_ref()
                )));
            }
            Ok(())
        }
    }
}

/// The contest's slate and NflData snapshot
struct SlateCtx {
    slate: Slate,
    snapshot: Arc<Snapshot>,
    now: OffsetDateTime,
}

impl SlateCtx {
    fn salaried_player(&self, value: &PlayerOrDefenseId) -> Option<&SalariedPlayer> {
        self.snapshot.salaried_player(&self.slate.id, value)
    }

    /// Whether the slate has kicked off.
    fn locked(&self) -> bool {
        self.slate.locked(self.now)
    }
}

async fn slate_ctx(state: &AppState, slate_id: SlateId) -> Result<SlateCtx, AppError> {
    let snapshot = state.nfl.snapshot();
    let slate = snapshot.slate(&slate_id).ok_or(AppError::NotFound)?.clone();
    Ok(SlateCtx {
        slate,
        snapshot,
        now: state.now(),
    })
}

/// The write-path gate: `Forbidden` once the slate kicks off.
async fn require_open(state: &AppState, slate_id: SlateId) -> Result<SlateCtx, AppError> {
    let ctx = slate_ctx(state, slate_id).await?;
    if ctx.locked() {
        return Err(AppError::Forbidden);
    }
    Ok(ctx)
}

/// A draft's cap position: how many slots are filled, what is left under the
/// cap, and the average left per open slot.
pub struct CapState {
    pub filled: u8,
    pub remaining: i64,
    pub per_open: i64,
}

impl CapState {
    fn new(draft: &DraftEntry, ctx: &SlateCtx) -> Self {
        let spent: i64 = RosterSlot::iter()
            .filter_map(|s| slot_salary(draft, s, ctx))
            .sum();
        let filled = RosterSlot::iter()
            .filter(|s| draft.get(*s).is_some())
            .count() as u8;
        let (remaining, per_open) = remaining_and_per_open(spent, filled);
        Self {
            filled,
            remaining,
            per_open,
        }
    }

    /// The save gate: every slot filled and the lineup under the cap.
    fn can_save(&self) -> bool {
        self.filled == 9 && self.remaining >= 0
    }

    /// Whether the lineup has spent past the cap.
    fn over_cap(&self) -> bool {
        self.remaining < 0
    }
}

/// The salary a filled slot contributes to the cap, `None` for an empty slot
fn slot_salary(draft: &DraftEntry, slot: RosterSlot, ctx: &SlateCtx) -> Option<i64> {
    draft
        .get(slot)
        .and_then(|value| ctx.salaried_player(&value))
        .map(|player| i64::from(player.salary))
}

fn build_slot_row(
    slot: RosterSlot,
    player: Option<&SalariedPlayer>,
    avatar: Option<&Media>,
) -> EditorSlotRow {
    EditorSlotRow {
        slot,
        info: player.map(|player| PickInfo::new(player.clone(), avatar)),
    }
}

/// Get-or-resume the draft row: read it off the reader, and only take the
/// serialized write connection to create it.
async fn get_or_create_draft(
    state: &AppState,
    slate_id: SlateId,
    team_id: FantasyTeamId,
    seed: Option<&Lineup>,
) -> Result<DraftEntry, AppError> {
    if let Some(draft) = draft_store::by_keys(state.db.reader(), slate_id, team_id).await? {
        return Ok(draft);
    }
    state
        .db
        .write_tx::<_, DraftEntry, AppError>(async move |conn| {
            Ok(draft_store::get_or_create_tx(conn, slate_id, team_id, seed).await?)
        })
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remaining_flips_negative_over_cap() {
        let (rem, _) = remaining_and_per_open(59_000, 1);
        assert_eq!(rem, 1_000);
    }

    #[test]
    fn per_open_is_floored_division() {
        let (rem, per) = remaining_and_per_open(30_000, 1);
        assert_eq!(rem, 30_000);
        assert_eq!(per, 3_750); // 30_000 / 8
    }

    #[test]
    fn per_open_is_zero_when_full() {
        let (rem, per) = remaining_and_per_open(0, 9);
        assert_eq!(rem, SALARY_CAP);
        assert_eq!(per, 0);
    }

    #[test]
    fn can_save_requires_nine_slots_and_nonnegative_remaining() {
        let cap = |filled, remaining| CapState {
            filled,
            remaining,
            per_open: 0,
        };
        assert!(cap(9, 0).can_save());
        assert!(cap(9, 1).can_save());
        assert!(!cap(9, -1).can_save(), "over cap disables save");
        assert!(!cap(8, 0).can_save(), "incomplete disables save");
        assert!(!cap(0, SALARY_CAP).can_save());
    }

    fn band_page(filled: u8, remaining: i64, locked: bool) -> EditorPage {
        EditorPage {
            contest_name: "Week 1".into(),
            team_name: "Team".into(),
            cap: CapState {
                filled,
                remaining,
                per_open: if filled == 9 {
                    0
                } else {
                    remaining / (9 - filled) as i64
                },
            },
            slots: Vec::new(),
            locked,
            entry_id: None,
        }
    }

    #[test]
    fn action_labels_follow_entry_state() {
        let mut page = band_page(9, 0, false);
        assert_eq!(page.save_label(), "Save Entry");
        assert_eq!(page.discard_label(), "Discard Draft");
        page.entry_id = Some(EntryId(42));
        assert_eq!(page.save_label(), "Update Entry");
        assert_eq!(page.discard_label(), "Discard Changes");
    }

    #[test]
    fn cap_note_tracks_the_band_state() {
        assert_eq!(
            band_page(4, 25_600, false).cap_note().as_deref(),
            Some("$5,120 per open")
        );
        assert_eq!(
            band_page(9, 2_200, false).cap_note().as_deref(),
            Some("All positions filled")
        );
        assert_eq!(
            band_page(9, -9_500, false).cap_note(),
            None,
            "over cap the red headline stands alone"
        );
    }

    #[test]
    fn save_hint_counts_the_open_slots() {
        assert_eq!(
            band_page(8, 100, false).save_hint().as_deref(),
            Some("1 position left to fill")
        );
        assert_eq!(
            band_page(4, 25_600, false).save_hint().as_deref(),
            Some("5 positions left to fill")
        );
        assert_eq!(
            band_page(9, -9_500, false).save_hint(),
            None,
            "over cap is the headline's story, not the hint's"
        );
        assert_eq!(band_page(9, 2_200, false).save_hint(), None, "saveable");
        assert_eq!(
            band_page(4, 0, true).save_hint(),
            None,
            "locked hides the hint"
        );
    }
}
