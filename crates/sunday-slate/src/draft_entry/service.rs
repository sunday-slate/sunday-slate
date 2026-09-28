//! The draft-entry editor's service layer: cap math, candidate pools, and the
//! page / slot / save operations the handler and templates consume.

use std::collections::{HashMap, HashSet};

use nfl_data::{Game, PlayerIdentity, Season, TeamAbbr as NflTeamAbbr, Week};
use strum::IntoEnumIterator;

use crate::avatars::Avatars;
use crate::contests::service::{slate_locked, slate_week};
use crate::contests::store as contest_store;
use crate::contests::{ContestId, resolve_games};
use crate::draft_entry::model::DraftEntry;
use crate::draft_entry::store as draft_store;
use crate::entries::service::{PickInfo, SALARY_CAP};
use crate::entries::store::{self as entries_store, SlateSalaries};
use crate::entries::{EntryId, EntrySlot, Lineup, NflPlayerId, RosterSlot, SlotValue};
use crate::fantasy_teams::FantasyTeamId;
use crate::player_salaries::model::{DfsPosition, NflPlayerSalary};
use crate::player_salaries::store as salary_store;
use crate::scoring::format_thousands;
use crate::scoring::service::season_averages;
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

/// The save gate: every slot filled and the lineup under the cap.
pub fn can_save(filled: u8, remaining: i64) -> bool {
    filled == 9 && remaining >= 0
}

/// The editor page: cap state, the nine rows, and whether the slate is read-only.
pub struct EditorPage {
    pub contest_name: String,
    pub team_name: String,
    pub remaining: i64,
    pub per_open: i64,
    pub filled: u8,
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
        can_save(self.filled, self.remaining)
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
        self.remaining < 0
    }

    /// The band's headline amount: what's left under the cap, or the
    /// overage (unsigned — the "over the salary cap" label carries the
    /// sign).
    pub fn remaining_str(&self) -> String {
        format!("${}", format_thousands(self.remaining.abs()))
    }

    /// The band's right-hand note: the per-open budget while slots stay
    /// open. `None` over the cap — the red headline already says it all.
    pub fn cap_note(&self) -> Option<String> {
        if self.remaining < 0 {
            None
        } else if self.filled == 9 {
            Some("All positions filled".to_string())
        } else {
            Some(format!("${} per open", format_thousands(self.per_open)))
        }
    }

    /// The open-slot count under the cap note, `None` once all are filled
    /// (or the lineup is locked and no longer editable).
    pub fn save_hint(&self) -> Option<String> {
        if self.locked {
            return None;
        }
        match 9 - self.filled {
            0 => None,
            1 => Some("1 position left to fill".to_string()),
            open => Some(format!("{open} positions left to fill")),
        }
    }
}

/// One of the editor's nine slot rows: what fills the slot and how it prices.
pub struct EditorSlotRow {
    pub slot: RosterSlot,
    pub label: &'static str,
    pub value: Option<SlotValue>,
    pub info: PickInfo,
    pub injury: Option<crate::injuries::InjuryDesignation>,
}

impl EditorSlotRow {
    pub fn injury(&self) -> Option<crate::injuries::InjuryChip> {
        self.injury.map(crate::injuries::chip)
    }
}

/// One candidate for a slot's pick list, with its price and pace.
pub struct SlotChoice {
    /// What the pick form submits: the candidate's `NflPlayerId` or `NflTeamAbbr`.
    pub key: SlotValue,
    pub position: String,
    pub is_picked: bool,
    /// Whether picking this candidate would put the lineup over the cap
    /// (their salary against the budget left by the *other* slots).
    pub over_cap: bool,
    /// The candidate's designation from the slate injury report.
    pub injury: Option<crate::injuries::InjuryDesignation>,
    pub info: PickInfo,
}

impl SlotChoice {
    /// The pick form's value: a player's gsis id, or a defense's team abbr.
    pub fn value_key(&self) -> &str {
        match &self.key {
            SlotValue::Player(id) => id.as_ref(),
            SlotValue::Defense(team) => &team.0,
        }
    }

    /// The row's injury badge, when the report has this candidate.
    pub fn injury(&self) -> Option<crate::injuries::InjuryChip> {
        self.injury.map(crate::injuries::chip)
    }
}

/// Open (or resume) the draft and assemble the editor page. `NotFound` when
/// the contest has no published slate.
pub async fn page(
    state: &AppState,
    team_id: FantasyTeamId,
    team_name: String,
    contest_id: ContestId,
) -> Result<EditorPage, AppError> {
    let reader = state.db.reader();

    let contest = contest_store::by_id(reader, contest_id)
        .await?
        .ok_or(AppError::NotFound)?;
    let ctx = slate_ctx(state, contest_id).await?;

    // Resume an in-progress draft; seed a first open from the committed entry.
    let committed = entries_store::lineup_for_team(reader, contest_id, team_id).await?;
    let entry_id = committed.as_ref().map(|(id, _)| *id);
    let seed = committed.and_then(|(_, lineup)| lineup);
    let draft = get_or_create_draft(state, contest_id, team_id, seed.as_ref()).await?;

    let salaries = pool_salaries(&ctx.pool);
    let ids = draft.filled_player_ids();
    let (names, averages) = player_profiles(state, &ids, ctx.week).await?;
    let player_ids: Vec<&str> = ids.iter().map(|id| id.as_ref()).collect();
    let team_abbrs: Vec<&str> = draft
        .def
        .as_ref()
        .map(|team| team.0.as_str())
        .into_iter()
        .collect();
    let avatars = crate::avatars::for_lineup(&state.db, &player_ids, &team_abbrs).await?;

    let slate_teams: HashMap<&str, &str> = ctx
        .pool
        .iter()
        .filter_map(|r| {
            r.gsis_player_id
                .as_deref()
                .map(|id| (id, r.team_abbr.as_str()))
        })
        .collect();

    let slots: Vec<EditorSlotRow> = RosterSlot::iter()
        .map(|s| {
            build_slot_row(
                &draft,
                s,
                &names,
                &averages,
                &salaries,
                &slate_teams,
                &avatars,
                &ctx.slate,
            )
        })
        .collect();

    let (filled, remaining, per_open) = cap_state(&draft, &salaries);

    let injuries_by_gsis = crate::live::slate_tuple(&ctx.slate)
        .and_then(|key| state.injuries.report(&key))
        .map(|report| report.by_gsis);
    let mut slots = slots;
    for row in &mut slots {
        if let Some(SlotValue::Player(id)) = &row.value
            && let Some(designation) = injuries_by_gsis.as_ref().and_then(|map| map.get(id))
        {
            row.injury = Some(*designation);
        }
    }

    Ok(EditorPage {
        contest_name: contest.name,
        team_name,
        remaining,
        per_open,
        filled,
        slots,
        locked: ctx.locked,
        entry_id,
    })
}

/// The candidate list for one slot, salary-descending. Omits players already
/// picked into another slot; the current occupant of `slot` appears marked
/// `is_picked`. Defense choices carry the team abbr as the value key.
pub async fn choices(
    state: &AppState,
    team_id: FantasyTeamId,
    contest_id: ContestId,
    slot: RosterSlot,
) -> Result<Vec<SlotChoice>, AppError> {
    // Resolve the slate first: a contest with no playable slate is `NotFound`,
    // and must not leave a draft row behind.
    let ctx = slate_ctx(state, contest_id).await?;
    let draft = get_or_create_draft(state, contest_id, team_id, None).await?;

    // The budget this slot may spend: the cap minus what the *other* slots
    // hold. Swapping replaces the current occupant, so it doesn't count.
    let salaries = pool_salaries(&ctx.pool);
    let injuries_by_gsis = crate::live::slate_tuple(&ctx.slate)
        .and_then(|key| state.injuries.report(&key))
        .map(|report| report.by_gsis);
    let spent_elsewhere: i64 = RosterSlot::iter()
        .filter(|s| *s != slot)
        .filter_map(|s| slot_salary(&draft, s, &salaries))
        .sum();
    let budget = SALARY_CAP - spent_elsewhere;

    let picked_elsewhere: HashSet<String> = RosterSlot::iter()
        .filter(|s| *s != slot)
        .filter_map(|s| match draft.get(s) {
            Some(SlotValue::Player(id)) => Some(id.0),
            _ => None,
        })
        .collect();
    let mut player_ids = Vec::new();
    let mut player_rows = Vec::new();
    if slot != RosterSlot::Def {
        for row in &ctx.pool {
            if !position_allowed(slot, row.dfs_position) {
                continue;
            }
            let Some(id) = row.gsis_player_id.as_deref() else {
                continue;
            };
            if picked_elsewhere.contains(id) {
                continue;
            }
            let index = player_ids.len();
            player_ids.push(NflPlayerId(id.to_string()));
            player_rows.push((row, index));
        }
    }
    let player_refs: Vec<&str> = player_ids.iter().map(|id| id.as_ref()).collect();
    // Only the D/ST list renders team logos.
    let team_refs: Vec<&str> = if slot == RosterSlot::Def {
        ctx.pool
            .iter()
            .map(|row| row.team_abbr.as_str())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect()
    } else {
        Vec::new()
    };
    let avatars = crate::avatars::for_lineup(&state.db, &player_refs, &team_refs).await?;
    let (names, averages) = if player_ids.is_empty() {
        (HashMap::new(), HashMap::new())
    } else {
        player_profiles(state, &player_ids, ctx.week).await?
    };

    let mut out: Vec<SlotChoice> = if slot == RosterSlot::Def {
        ctx.pool
            .iter()
            .filter(|r| r.dfs_position.is_defense())
            .map(|row| {
                let team = NflTeamAbbr(row.team_abbr.clone());
                SlotChoice {
                    position: "D/ST".into(),
                    is_picked: draft.def.as_ref() == Some(&team),
                    over_cap: row.salary > budget,
                    injury: None,
                    info: PickInfo::defense(
                        &team,
                        avatars.teams.get(team.0.as_str()),
                        &ctx.slate,
                        Some(row.salary),
                    ),
                    key: SlotValue::Defense(team),
                }
            })
            .collect()
    } else {
        let same_slot = match draft.get(slot) {
            Some(SlotValue::Player(id)) => Some(id),
            _ => None,
        };

        player_rows
            .into_iter()
            .map(|(row, index)| {
                let gsis = player_ids[index].clone();
                let identity = names.get(gsis.as_ref());
                SlotChoice {
                    position: row.dfs_position.label().into(),
                    is_picked: same_slot.as_ref() == Some(&gsis),
                    over_cap: row.salary > budget,
                    injury: injuries_by_gsis
                        .as_ref()
                        .and_then(|map| map.get(&gsis))
                        .copied(),
                    info: PickInfo::player(
                        identity
                            .map(|i| i.name.clone())
                            .unwrap_or_else(|| gsis.as_ref().to_string()),
                        avatars.players.get(gsis.as_ref()),
                        Some(row.team_abbr.clone()),
                        &ctx.slate,
                        Some(row.salary),
                        averages.get(gsis.as_ref()).copied(),
                    ),
                    key: SlotValue::Player(gsis),
                }
            })
            .collect()
    };

    out.sort_by(|a: &SlotChoice, b: &SlotChoice| {
        b.info
            .salary
            .cmp(&a.info.salary)
            .then_with(|| a.info.name.cmp(&b.info.name))
    });
    Ok(out)
}

/// Place (or replace) a pick in a slot. Rejects an invalid pick (see
/// [`validate_pick`]) and refuses on a locked slate.
pub async fn set_slot(
    state: &AppState,
    team_id: FantasyTeamId,
    contest_id: ContestId,
    slot: RosterSlot,
    value: SlotValue,
) -> Result<(), AppError> {
    let ctx = require_open(state, contest_id).await?;
    mutate_draft(state, contest_id, team_id, move |draft| {
        validate_pick(draft, slot, &value, &ctx.pool)?;
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
    contest_id: ContestId,
    slot: RosterSlot,
) -> Result<(), AppError> {
    require_open(state, contest_id).await?;
    mutate_draft(state, contest_id, team_id, move |draft| {
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
    contest_id: ContestId,
    team_id: FantasyTeamId,
    mutate: impl FnOnce(&mut DraftEntry) -> Result<(), AppError> + Send,
) -> Result<(), AppError> {
    state
        .db
        .write_tx::<_, (), AppError>(async move |conn| {
            let mut draft = draft_store::get_or_create_tx(conn, contest_id, team_id, None).await?;
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
    contest_id: ContestId,
) -> Result<(), AppError> {
    state
        .db
        .write_tx::<_, (), AppError>(async move |conn| {
            draft_store::delete_by_keys(conn, contest_id, team_id).await?;
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
    contest_id: ContestId,
) -> Result<EntryId, AppError> {
    let ctx = require_open(state, contest_id).await?;
    let salaries = pool_salaries(&ctx.pool);
    state
        .db
        .write_tx::<_, EntryId, AppError>(async move |conn| {
            let Some(draft) = draft_store::by_keys(&mut *conn, contest_id, team_id).await? else {
                return Err(AppError::BadRequest(
                    "Complete all nine slots before saving.".into(),
                ));
            };
            let (filled, remaining, _) = cap_state(&draft, &salaries);
            if !can_save(filled, remaining) {
                return Err(AppError::BadRequest(if filled < 9 {
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
            let id = entries_store::upsert_with_lineup(conn, contest_id, team_id, &lineup).await?;
            draft_store::delete(&mut *conn, draft.id).await?;
            Ok(id)
        })
        .await
}

/// Whether a position may fill a slot. FLEX is the RB/WR/TE union; a defense
/// is never a player position.
fn position_allowed(slot: RosterSlot, position: DfsPosition) -> bool {
    match slot {
        RosterSlot::Qb => position == DfsPosition::Qb,
        RosterSlot::Rb1 | RosterSlot::Rb2 => position == DfsPosition::Rb,
        RosterSlot::Wr1 | RosterSlot::Wr2 | RosterSlot::Wr3 => position == DfsPosition::Wr,
        RosterSlot::Te => position == DfsPosition::Te,
        RosterSlot::Flex => matches!(
            position,
            DfsPosition::Rb | DfsPosition::Wr | DfsPosition::Te
        ),
        RosterSlot::Def => false,
    }
}

/// The picker only offers valid choices, so these rejections are the
/// defense-in-depth path: a stale or tampered form fails with a message
/// instead of a constraint error.
fn validate_pick(
    draft: &DraftEntry,
    slot: RosterSlot,
    value: &SlotValue,
    pool: &[NflPlayerSalary],
) -> Result<(), AppError> {
    match value {
        SlotValue::Defense(team) => {
            if slot != RosterSlot::Def {
                return Err(AppError::BadRequest(format!(
                    "{} is a defense; the {} slot takes a player.",
                    team.0,
                    slot.position_label()
                )));
            }
            if !pool
                .iter()
                .any(|r| r.dfs_position.is_defense() && r.team_abbr == team.0)
            {
                return Err(AppError::BadRequest(format!(
                    "{} is not on this slate.",
                    team.0
                )));
            }
            Ok(())
        }
        SlotValue::Player(id) => {
            if slot == RosterSlot::Def {
                return Err(AppError::BadRequest(
                    "DEF takes a team defense, not a player.".into(),
                ));
            }
            let position = pool
                .iter()
                .find(|r| r.gsis_player_id.as_deref() == Some(id.as_ref()))
                .map(|r| r.dfs_position)
                .ok_or_else(|| {
                    AppError::BadRequest(format!("{} is not on this slate.", id.as_ref()))
                })?;
            if !position_allowed(slot, position) {
                return Err(AppError::BadRequest(format!(
                    "{} does not fit the {} slot.",
                    id.as_ref(),
                    slot.position_label()
                )));
            }
            let elsewhere = RosterSlot::iter()
                .filter(|s| *s != slot)
                .filter_map(|s| match draft.get(s) {
                    Some(SlotValue::Player(p)) => Some(p),
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

/// Names and season pace for a set of ids.
async fn player_profiles(
    state: &AppState,
    ids: &[NflPlayerId],
    week: Week,
) -> Result<(HashMap<String, PlayerIdentity>, HashMap<String, (f64, u32)>), AppError> {
    let names = state.nfl.identify(ids).await?;
    let averages = season_averages(&state.nfl, Season(state.config.season), ids, week).await?;
    Ok((names, averages))
}

/// The contest's published slate, its lock state, and its salary pool —
/// everything a draft operation needs about the slate, loaded once per
/// request. `NotFound` when nothing is published (an empty slate has no week).
struct SlateCtx {
    slate: Vec<Game>,
    week: Week,
    locked: bool,
    /// Every priced player and defense on the contest's games.
    pool: Vec<NflPlayerSalary>,
}

async fn slate_ctx(state: &AppState, contest_id: ContestId) -> Result<SlateCtx, AppError> {
    let reader = state.db.reader();
    let game_ids = contest_store::game_ids(reader, contest_id).await?;
    let season_games = state.nfl.games(Season(state.config.season)).await?;
    let slate = resolve_games(&game_ids, &season_games);
    let week = slate_week(&slate).ok_or(AppError::NotFound)?;
    let priced: Vec<&str> = game_ids.iter().map(String::as_str).collect();
    let pool = salary_store::for_games(reader, &priced).await?;
    Ok(SlateCtx {
        locked: slate_locked(&slate, state.now()),
        slate,
        week,
        pool,
    })
}

/// The write-path gate: `Forbidden` once the slate kicks off.
async fn require_open(state: &AppState, contest_id: ContestId) -> Result<SlateCtx, AppError> {
    let ctx = slate_ctx(state, contest_id).await?;
    if ctx.locked {
        return Err(AppError::Forbidden);
    }
    Ok(ctx)
}

/// The pool's prices keyed for cap math.
fn pool_salaries(pool: &[NflPlayerSalary]) -> SlateSalaries {
    let mut salaries = SlateSalaries::default();
    for row in pool {
        salaries.insert_row(
            row.dfs_position.is_defense(),
            row.gsis_player_id.clone().map(NflPlayerId),
            NflTeamAbbr(row.team_abbr.clone()),
            row.salary,
        );
    }
    salaries
}

/// Filled-slot count, remaining budget, and per-open average for a draft.
fn cap_state(draft: &DraftEntry, salaries: &SlateSalaries) -> (u8, i64, i64) {
    let spent: i64 = RosterSlot::iter()
        .filter_map(|s| slot_salary(draft, s, salaries))
        .sum();
    let filled = RosterSlot::iter()
        .filter(|s| draft.get(*s).is_some())
        .count() as u8;
    let (remaining, per_open) = remaining_and_per_open(spent, filled);
    (filled, remaining, per_open)
}

/// The salary a filled slot contributes to the cap, `None` for an empty slot
/// (an unpriced pick — an import gap — counts as `$0`).
fn slot_salary(draft: &DraftEntry, slot: RosterSlot, salaries: &SlateSalaries) -> Option<i64> {
    match draft.get(slot) {
        Some(SlotValue::Player(id)) => salaries.players.get(&id).copied(),
        Some(SlotValue::Defense(team)) => salaries.defenses.get(&team).copied(),
        None => None,
    }
}

#[allow(clippy::too_many_arguments)]
fn build_slot_row(
    draft: &DraftEntry,
    slot: RosterSlot,
    names: &HashMap<String, PlayerIdentity>,
    averages: &HashMap<String, (f64, u32)>,
    salaries: &SlateSalaries,
    slate_teams: &HashMap<&str, &str>,
    avatars: &Avatars,
    slate: &[Game],
) -> EditorSlotRow {
    let value = draft.get(slot);
    let salary = slot_salary(draft, slot, salaries);
    let info = match &value {
        Some(SlotValue::Player(id)) => {
            let identity = names.get(id.as_ref());
            // The slate's salary row names the pick's team for this week; a
            // player's identity may carry a later team, so it is only the
            // fallback.
            let team = slate_teams
                .get(id.as_ref())
                .map(|t| t.to_string())
                .or_else(|| identity.and_then(|i| i.team.as_ref()).map(|t| t.0.clone()));
            PickInfo::player(
                identity
                    .map(|i| i.name.clone())
                    .unwrap_or_else(|| id.as_ref().to_string()),
                avatars.players.get(id.as_ref()),
                team,
                slate,
                salary,
                averages.get(id.as_ref()).copied(),
            )
        }
        Some(SlotValue::Defense(t)) => {
            PickInfo::defense(t, avatars.teams.get(t.0.as_str()), slate, salary)
        }
        None => PickInfo::empty(),
    };
    EditorSlotRow {
        slot,
        label: slot.position_label(),
        value,
        info,
        injury: None,
    }
}

/// Get-or-resume the draft row: read it off the reader, and only take the
/// serialized write connection to create it.
async fn get_or_create_draft(
    state: &AppState,
    contest_id: ContestId,
    team_id: FantasyTeamId,
    seed: Option<&Lineup>,
) -> Result<DraftEntry, AppError> {
    if let Some(draft) = draft_store::by_keys(state.db.reader(), contest_id, team_id).await? {
        return Ok(draft);
    }
    state
        .db
        .write_tx::<_, DraftEntry, AppError>(async move |conn| {
            Ok(draft_store::get_or_create_tx(conn, contest_id, team_id, seed).await?)
        })
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::draft_entry::model::DraftEntryId;
    use crate::tests::TestApp;
    use crate::tests::factories::{self, TeamOptions};
    use sqlx::SqlitePool;

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
        assert!(can_save(9, 0));
        assert!(can_save(9, 1));
        assert!(!can_save(9, -1), "over cap disables save");
        assert!(!can_save(8, 0), "incomplete disables save");
        assert!(!can_save(0, SALARY_CAP));
    }

    fn band_page(filled: u8, remaining: i64, locked: bool) -> EditorPage {
        EditorPage {
            contest_name: "Week 1".into(),
            team_name: "Team".into(),
            remaining,
            per_open: if filled == 9 {
                0
            } else {
                remaining / (9 - filled) as i64
            },
            filled,
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

    #[test]
    fn flex_accepts_rb_wr_te_but_not_qb() {
        assert!(position_allowed(RosterSlot::Flex, DfsPosition::Rb));
        assert!(position_allowed(RosterSlot::Flex, DfsPosition::Wr));
        assert!(position_allowed(RosterSlot::Flex, DfsPosition::Te));
        assert!(!position_allowed(RosterSlot::Flex, DfsPosition::Qb));
        assert!(!position_allowed(RosterSlot::Def, DfsPosition::Rb));
    }

    fn empty_draft() -> DraftEntry {
        DraftEntry {
            id: DraftEntryId(1),
            contest_id: ContestId(1),
            fantasy_team_id: FantasyTeamId(1),
            qb: None,
            rb1: None,
            rb2: None,
            wr1: None,
            wr2: None,
            wr3: None,
            te: None,
            flex: None,
            def: None,
        }
    }

    fn salary(gsis: &str, position: DfsPosition) -> NflPlayerSalary {
        NflPlayerSalary {
            id: 0,
            gsis_game_id: "2025_01_BUF_KC".into(),
            gsis_player_id: Some(gsis.into()),
            team_abbr: "KC".into(),
            dfs_position: position,
            salary: 5_000,
        }
    }

    fn pool() -> Vec<NflPlayerSalary> {
        vec![
            salary("00-QB", DfsPosition::Qb),
            salary("00-RB", DfsPosition::Rb),
            salary("00-WR", DfsPosition::Wr),
        ]
    }

    #[test]
    fn set_slot_rejects_a_qb_in_flex_and_accepts_an_rb() {
        let draft = empty_draft();
        let pool = pool();

        assert!(
            validate_pick(
                &draft,
                RosterSlot::Flex,
                &SlotValue::Player(NflPlayerId("00-RB".into())),
                &pool
            )
            .is_ok(),
            "an RB fills FLEX"
        );
        let err = validate_pick(
            &draft,
            RosterSlot::Flex,
            &SlotValue::Player(NflPlayerId("00-QB".into())),
            &pool,
        )
        .unwrap_err();
        assert!(
            matches!(err, AppError::BadRequest(_)),
            "a QB does not fill FLEX"
        );
    }

    #[test]
    fn set_slot_rejects_off_slate_defense_and_duplicate_picks() {
        let mut draft = empty_draft();
        draft.set(
            RosterSlot::Qb,
            SlotValue::Player(NflPlayerId("00-QB".into())),
        );
        let pool = pool();

        // A defense into a player slot.
        let err = validate_pick(
            &draft,
            RosterSlot::Rb1,
            &SlotValue::Defense(NflTeamAbbr("KC".into())),
            &pool,
        )
        .unwrap_err();
        assert!(matches!(err, AppError::BadRequest(_)));

        // An id with no salary row (off slate).
        let err = validate_pick(
            &draft,
            RosterSlot::Rb1,
            &SlotValue::Player(NflPlayerId("00-NOPE".into())),
            &pool,
        )
        .unwrap_err();
        assert!(matches!(err, AppError::BadRequest(_)));

        // The already-picked QB into another slot.
        let err = validate_pick(
            &draft,
            RosterSlot::Rb1,
            &SlotValue::Player(NflPlayerId("00-QB".into())),
            &pool,
        )
        .unwrap_err();
        assert!(matches!(err, AppError::BadRequest(_)));

        // An off-slate defense is not on the slate.
        let err = validate_pick(
            &empty_draft(),
            RosterSlot::Def,
            &SlotValue::Defense(NflTeamAbbr("CAR".into())),
            &pool,
        )
        .unwrap_err();
        assert!(matches!(err, AppError::BadRequest(_)));
    }

    fn complete_lineup() -> Lineup {
        Lineup {
            qb: NflPlayerId("00-Q".into()),
            rb1: NflPlayerId("00-R1".into()),
            rb2: NflPlayerId("00-R2".into()),
            wr1: NflPlayerId("00-W1".into()),
            wr2: NflPlayerId("00-W2".into()),
            wr3: NflPlayerId("00-W3".into()),
            te: NflPlayerId("00-T".into()),
            flex: NflPlayerId("00-F".into()),
            def: NflTeamAbbr("KC".into()),
        }
    }

    /// Test-side get-or-create through the tx-only store path.
    async fn create_draft(
        pool: &SqlitePool,
        contest_id: ContestId,
        team_id: FantasyTeamId,
    ) -> DraftEntry {
        let mut conn = pool.acquire().await.unwrap();
        draft_store::get_or_create_tx(&mut conn, contest_id, team_id, None)
            .await
            .unwrap()
    }

    async fn fill_draft(pool: &SqlitePool, contest_id: ContestId, team_id: FantasyTeamId) {
        let lineup = complete_lineup();
        let mut draft = create_draft(pool, contest_id, team_id).await;
        for (slot, id) in lineup.player_slots() {
            draft.set(slot, SlotValue::Player(id.clone()));
        }
        draft.set(RosterSlot::Def, SlotValue::Defense(lineup.def.clone()));
        draft_store::save(pool, &draft).await.unwrap();
    }

    #[sqlx::test]
    async fn save_refuses_incomplete_and_commits_a_full_draft(pool: SqlitePool) {
        let app = TestApp::from_pool(pool.clone()).await;
        let fixture = factories::team(&pool, TeamOptions::default()).await;
        let contest = factories::contest_with_games(&pool, "Week 1", &["2025_01_BUF_KC"]).await;
        app.nfl
            .seed_for_test(&[], &[factories::game("2025_01_BUF_KC", 1)])
            .await
            .unwrap();

        let team_id = FantasyTeamId(fixture.team.id);
        let contest_id = ContestId(contest);

        // An empty draft row exists (resume path).
        create_draft(&pool, contest_id, team_id).await;

        // Save refuses: fewer than nine slots filled.
        let err = save(&app.state(), team_id, contest_id).await.unwrap_err();
        assert!(
            matches!(err, AppError::BadRequest(_)),
            "incomplete refuses: {err:?}"
        );

        // A full draft commits: one entry, nine slots, the draft row gone.
        fill_draft(&pool, contest_id, team_id).await;
        let entry_id = save(&app.state(), team_id, contest_id)
            .await
            .expect("full draft saves");
        let slots: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM entry_slots WHERE entry_id = ?1")
            .bind(entry_id.0)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(slots, 9);
        let drafts: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM draft_entries WHERE contest_id = ?1 AND fantasy_team_id = ?2",
        )
        .bind(contest_id)
        .bind(team_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(drafts, 0);
    }

    #[sqlx::test]
    async fn save_refuses_a_full_lineup_over_cap(pool: SqlitePool) {
        let app = TestApp::from_pool(pool.clone()).await;
        let fixture = factories::team(&pool, TeamOptions::default()).await;
        let contest = factories::contest_with_games(&pool, "Week 1", &["2025_01_BUF_KC"]).await;
        sqlx::query(
            "INSERT INTO nfl_player_salaries (gsis_game_id, gsis_player_id, team_abbr, dfs_position, salary)
             VALUES ('2025_01_BUF_KC', '00-Q', 'KC', 'QB', 60001)",
        )
        .execute(&pool)
        .await
        .unwrap();
        app.nfl
            .seed_for_test(&[], &[factories::game("2025_01_BUF_KC", 1)])
            .await
            .unwrap();

        let team_id = FantasyTeamId(fixture.team.id);
        let contest_id = ContestId(contest);
        fill_draft(&pool, contest_id, team_id).await;

        let err = save(&app.state(), team_id, contest_id).await.unwrap_err();
        assert!(
            matches!(err, AppError::BadRequest(_)),
            "over cap refuses: {err:?}"
        );
        let entries: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM entries WHERE contest_id = ?1 AND fantasy_team_id = ?2",
        )
        .bind(contest_id)
        .bind(team_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(entries, 0, "nothing written when refused");
    }

    #[sqlx::test]
    async fn choices_omit_picked_ids_and_mark_the_same_slot_pick(pool: SqlitePool) {
        let app = TestApp::from_pool(pool.clone()).await;
        let fixture = factories::team(&pool, TeamOptions::default()).await;
        let contest = factories::contest_with_games(&pool, "Week 1", &["2025_01_BUF_KC"]).await;
        for (gsis, position, salary) in [
            ("00-Q", "QB", 8_000),
            ("00-R1", "RB", 5_700),
            ("00-R2", "RB", 5_500),
        ] {
            sqlx::query(
                "INSERT INTO nfl_player_salaries (gsis_game_id, gsis_player_id, team_abbr, dfs_position, salary)
                 VALUES ('2025_01_BUF_KC', ?1, 'KC', ?2, ?3)",
            )
            .bind(gsis)
            .bind(position)
            .bind(salary)
            .execute(&pool)
            .await
            .unwrap();
        }
        let players = [
            factories::player("00-Q", "Quinn Back"),
            factories::player("00-R1", "Alpha Runner"),
            factories::player("00-R2", "Beta Runner"),
        ];
        app.nfl
            .seed_for_test(&players, &[factories::game("2025_01_BUF_KC", 1)])
            .await
            .unwrap();

        let team_id = FantasyTeamId(fixture.team.id);
        let contest_id = ContestId(contest);

        // Pick 00-R1 into RB1.
        let mut draft = create_draft(&pool, contest_id, team_id).await;
        draft.set(
            RosterSlot::Rb1,
            SlotValue::Player(NflPlayerId("00-R1".into())),
        );
        draft_store::save(&pool, &draft).await.unwrap();

        // The RB1 list keeps the current pick (marked) and filters the QB out.
        let rb_choices = choices(&app.state(), team_id, contest_id, RosterSlot::Rb1)
            .await
            .expect("rb1 choices");
        let rb_ids: Vec<&str> = rb_choices.iter().map(|c| c.value_key()).collect();
        assert_eq!(rb_ids, ["00-R1", "00-R2"]);
        assert!(
            rb_choices
                .iter()
                .find(|c| c.value_key() == "00-R1")
                .unwrap()
                .is_picked,
            "the same-slot pick is marked"
        );
        assert!(
            !rb_choices
                .iter()
                .find(|c| c.value_key() == "00-R2")
                .unwrap()
                .is_picked
        );

        // A different slot's list omits the already-picked id.
        let qb_choices = choices(&app.state(), team_id, contest_id, RosterSlot::Qb)
            .await
            .expect("qb choices");
        let qb_ids: Vec<&str> = qb_choices.iter().map(|c| c.value_key()).collect();
        assert_eq!(qb_ids, ["00-Q"]);
        assert!(
            !qb_choices[0].is_picked,
            "an empty slot has no current pick"
        );
    }

    /// Resolving the slate gates the draft. A contest whose games are absent
    /// from the nfl cache has no week, so the request is `NotFound` — and must
    /// not leave a draft row behind for a slate that cannot be played.
    #[sqlx::test]
    async fn choices_on_an_unresolvable_slate_creates_no_draft(pool: SqlitePool) {
        let app = TestApp::from_pool(pool.clone()).await;
        let fixture = factories::team(&pool, TeamOptions::default()).await;
        // The contest's game is never seeded into the nfl cache, so the slate
        // resolves to nothing and carries no week.
        let contest = factories::contest_with_games(&pool, "Week 1", &["2025_01_BUF_KC"]).await;

        let team_id = FantasyTeamId(fixture.team.id);
        let contest_id = ContestId(contest);

        match choices(&app.state(), team_id, contest_id, RosterSlot::Qb).await {
            Err(AppError::NotFound) => {}
            Err(e) => panic!("expected NotFound, got {e:?}"),
            Ok(list) => panic!("expected NotFound, got {} choices", list.len()),
        }

        let drafts: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM draft_entries WHERE contest_id = ?1 AND fantasy_team_id = ?2",
        )
        .bind(contest_id)
        .bind(team_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(drafts, 0, "no draft row for an unplayable slate");
    }

    /// `Db::test` aliases read and write to one pool, which masked the bug
    /// where the draft INSERT ran on the read-only reader. A production-shaped
    /// `Db` (read-only reader) proves `page` creates the draft through the
    /// write pool.
    #[sqlx::test]
    async fn get_or_create_routes_through_the_write_pool(pool: SqlitePool) {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock after epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("slate-draft-{unique}.db"));
        let db = crate::Db::open(path.to_str().expect("utf-8 temp path"))
            .await
            .expect("open production-shaped db");

        // Seed the smallest valid fixture set through the serialized writer.
        let (contest_id, team_id) = db
            .write_tx::<_, (i64, i64), sqlx::Error>(async move |conn| {
                let user_id: i64 = sqlx::query_scalar(
                    "INSERT INTO users (email, password_hash)
                     VALUES ('owner@example.com', 'hash') RETURNING id",
                )
                .fetch_one(&mut *conn)
                .await?;
                let league_id: i64 =
                    sqlx::query_scalar("INSERT INTO leagues (name) VALUES ('League') RETURNING id")
                        .fetch_one(&mut *conn)
                        .await?;
                let team_id: i64 = sqlx::query_scalar(
                    "INSERT INTO fantasy_teams (league_id, user_id, name, owner_name)
                     VALUES (?1, ?2, 'Team', 'Owner') RETURNING id",
                )
                .bind(league_id)
                .bind(user_id)
                .fetch_one(&mut *conn)
                .await?;
                let contest_id: i64 = sqlx::query_scalar(
                    "INSERT INTO contests (name) VALUES ('Week 1') RETURNING id",
                )
                .fetch_one(&mut *conn)
                .await?;
                sqlx::query("INSERT INTO contest_games (contest_id, gsis_game_id) VALUES (?1, '2025_01_BUF_KC')")
                    .bind(contest_id)
                    .execute(&mut *conn)
                    .await?;
                Ok((contest_id, team_id))
            })
            .await
            .expect("seed fixtures");

        // Surround the production Db with a TestApp's config, sessions, and nfl.
        let app = TestApp::from_pool(pool).await;
        app.nfl
            .seed_for_test(&[], &[factories::game("2025_01_BUF_KC", 1)])
            .await
            .unwrap();
        let state = AppState { db, ..app.state() };

        // Opening the editor creates the (absent) draft row. It must write
        // through the write pool — the reader would reject the INSERT.
        let page = page(
            &state,
            FantasyTeamId(team_id),
            "Team".into(),
            ContestId(contest_id),
        )
        .await
        .expect("creating a draft writes through the write pool");
        assert_eq!(page.filled, 0, "a fresh draft is empty");

        // The committed row is visible through the read-only reader after.
        let draft = draft_store::by_keys(
            state.db.reader(),
            ContestId(contest_id),
            FantasyTeamId(team_id),
        )
        .await
        .expect("query the reader")
        .expect("draft row is committed");
        assert_eq!(draft.filled_player_ids().len(), 0);

        drop(state);
        for suffix in ["", "-wal", "-shm"] {
            let mut p = path.clone().into_os_string();
            p.push(suffix);
            let _ = std::fs::remove_file(p);
        }
    }

    #[sqlx::test]
    async fn clear_slot_empties_a_just_set_slot(pool: SqlitePool) {
        let app = TestApp::from_pool(pool.clone()).await;
        let fixture = factories::team(&pool, TeamOptions::default()).await;
        let contest = factories::contest_with_games(&pool, "Week 1", &["2025_01_BUF_KC"]).await;
        app.nfl
            .seed_for_test(&[], &[factories::game("2025_01_BUF_KC", 1)])
            .await
            .unwrap();

        let team_id = FantasyTeamId(fixture.team.id);
        let contest_id = ContestId(contest);

        // A just-set QB pick (plus a neighbor), persisted.
        let mut draft = create_draft(&pool, contest_id, team_id).await;
        draft.set(
            RosterSlot::Qb,
            SlotValue::Player(NflPlayerId("00-Q".into())),
        );
        draft.set(
            RosterSlot::Rb1,
            SlotValue::Player(NflPlayerId("00-R".into())),
        );
        draft_store::save(&pool, &draft).await.unwrap();

        clear_slot(&app.state(), team_id, contest_id, RosterSlot::Qb)
            .await
            .expect("a filled slot clears");

        let after = draft_store::by_keys(&pool, contest_id, team_id)
            .await
            .unwrap()
            .expect("draft row kept");
        assert!(after.qb.is_none(), "the cleared slot is empty");
        assert!(
            after.rb1.is_some(),
            "clearing one slot leaves the others alone"
        );
        let rows: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM draft_entries WHERE contest_id = ?1 AND fantasy_team_id = ?2",
        )
        .bind(contest_id)
        .bind(team_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(rows, 1, "clearing keeps the draft row for the resume path");
    }
}
