//! The draft-entry editor's HTTP handlers: the editor page, the per-slot
//! pick page, and the set / clear / save / discard mutations.

use std::str::FromStr;

use askama::Template;
use axum::Router;
use axum::extract::{Path, State};
use axum::response::{Html, Response};
use axum::routing::{get, post};
use axum_htmx::HxRequest;
use nfl_data::TeamAbbr as NflTeamAbbr;
use serde::Deserialize;

use crate::chrome::Chrome;
use crate::contests::ContestId;
use crate::draft_entry::service::{self, EditorPage, SlotChoice};
use crate::entries::{NflPlayerId, RosterSlot, SlotValue};
use crate::fantasy_teams::{CurrentTeam, FantasyTeamId};
use crate::web::redirect;
use crate::{AppError, AppState};

/// Routes for the draft-entry editor.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/contests/{id}/draft-entry", get(editor))
        .route(
            "/contests/{id}/draft-entry/{slot}",
            get(pick).post(set_slot),
        )
        .route("/contests/{id}/draft-entry/{slot}/clear", post(clear_slot))
        .route("/contests/{id}/draft-entry/save", post(save))
        .route("/contests/{id}/draft-entry/discard", post(discard))
}

/// The `{slot}` path segment: the strum names are UPPERCASE-only.
fn parse_slot(slot: &str) -> Result<RosterSlot, AppError> {
    RosterSlot::from_str(slot).map_err(|_| AppError::NotFound)
}

#[derive(Template)]
#[template(path = "draft_entry/editor.html")]
struct EditorView {
    page: EditorPage,
    chrome: Chrome,
    contest_id: i64,
}

/// The full editor page: cap band, the nine slot rows, save/discard footer.
pub async fn editor(
    State(state): State<AppState>,
    CurrentTeam(team): CurrentTeam,
    Path(id): Path<i64>,
) -> Result<Html<String>, AppError> {
    let page = service::page(&state, FantasyTeamId(team.id), team.name, ContestId(id)).await?;
    let chrome = Chrome::focused("Edit Lineup", format!("/contests/{id}"));
    Ok(Html(
        EditorView {
            page,
            chrome,
            contest_id: id,
        }
        .render()?,
    ))
}

#[derive(Template)]
#[template(path = "draft_entry/picks.html")]
struct PickView {
    chrome: Chrome,
    contest_id: i64,
    slot: RosterSlot,
    choices: Vec<SlotChoice>,
}

impl PickView {
    /// True for the DEF slot: choices POST back `team_abbr` instead of
    /// `gsis_id`.
    fn is_def(&self) -> bool {
        self.slot == RosterSlot::Def
    }

    /// What the search field invites you to type. FLEX and DEF get words
    /// because "Search FLEXs" and "Search DEFs" don't read as English.
    fn search_placeholder(&self) -> String {
        match self.slot {
            RosterSlot::Flex => "Search players".into(),
            RosterSlot::Def => "Search defenses".into(),
            slot => format!("Search {}s", slot.position_label()),
        }
    }

    /// The empty state's noun: "No defenses match ..." for DEF.
    fn search_noun(&self) -> &'static str {
        if self.is_def() { "defenses" } else { "players" }
    }
}

/// The candidate list for one slot, its own page: "Select QB" in the bar and
/// an X that closes back to the editor.
pub async fn pick(
    State(state): State<AppState>,
    CurrentTeam(team): CurrentTeam,
    Path((id, slot)): Path<(i64, String)>,
) -> Result<Html<String>, AppError> {
    let slot = parse_slot(&slot)?;
    let choices = service::choices(&state, FantasyTeamId(team.id), ContestId(id), slot).await?;
    let chrome = Chrome::focused(
        format!("Select {}", slot.position_label()),
        format!("/contests/{id}/draft-entry"),
    );
    Ok(Html(
        PickView {
            chrome,
            contest_id: id,
            slot,
            choices,
        }
        .render()?,
    ))
}

#[derive(Deserialize, Default)]
pub struct SetSlotForm {
    gsis_id: Option<String>,
    team_abbr: Option<String>,
}

/// Place a pick in a slot (`gsis_id` for players, `team_abbr` for DEF), then
/// back to the editor.
pub async fn set_slot(
    HxRequest(is_htmx): HxRequest,
    State(state): State<AppState>,
    CurrentTeam(team): CurrentTeam,
    Path((id, slot)): Path<(i64, String)>,
    axum::Form(form): axum::Form<SetSlotForm>,
) -> Result<Response, AppError> {
    let slot = parse_slot(&slot)?;
    let contest_id = ContestId(id);
    let value = if slot == RosterSlot::Def {
        let abbr = form
            .team_abbr
            .ok_or_else(|| AppError::BadRequest("DEF takes a team_abbr.".into()))?;
        SlotValue::Defense(NflTeamAbbr(abbr))
    } else {
        let gsis = form
            .gsis_id
            .ok_or_else(|| AppError::BadRequest("Player slots take a gsis_id.".into()))?;
        SlotValue::Player(NflPlayerId(gsis))
    };
    service::set_slot(&state, FantasyTeamId(team.id), contest_id, slot, value).await?;
    Ok(redirect(is_htmx, format!("/contests/{id}/draft-entry")))
}

/// Empty a slot, removing its pick from the draft.
pub async fn clear_slot(
    HxRequest(is_htmx): HxRequest,
    State(state): State<AppState>,
    CurrentTeam(team): CurrentTeam,
    Path((id, slot)): Path<(i64, String)>,
) -> Result<Response, AppError> {
    let slot = parse_slot(&slot)?;
    service::clear_slot(&state, FantasyTeamId(team.id), ContestId(id), slot).await?;
    Ok(redirect(is_htmx, format!("/contests/{id}/draft-entry")))
}

/// Commit the draft as a real entry, then go to it.
pub async fn save(
    HxRequest(is_htmx): HxRequest,
    State(state): State<AppState>,
    CurrentTeam(team): CurrentTeam,
    Path(id): Path<i64>,
) -> Result<Response, AppError> {
    let entry_id = service::save(&state, FantasyTeamId(team.id), ContestId(id)).await?;
    Ok(redirect(is_htmx, format!("/entries/{}", entry_id.0)))
}

/// Abandon the draft and return to the contest.
pub async fn discard(
    HxRequest(is_htmx): HxRequest,
    State(state): State<AppState>,
    CurrentTeam(team): CurrentTeam,
    Path(id): Path<i64>,
) -> Result<Response, AppError> {
    service::discard(&state, FantasyTeamId(team.id), ContestId(id)).await?;
    Ok(redirect(is_htmx, format!("/contests/{id}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::draft_entry::service::EditorSlotRow;
    use crate::entries::service::PickInfo;
    use crate::entries::{RosterSlot, SlotValue};
    use crate::injuries::InjuryDesignation;
    use strum::IntoEnumIterator;

    /// A named pick priced at `salary`, kicking off at 1:00pm.
    fn player_info(name: &str, salary: i64, pace: Option<(f64, u32)>) -> PickInfo {
        let mut info = PickInfo::player(
            name.into(),
            None,
            Some("KC".into()),
            &[],
            Some(salary),
            pace,
        );
        info.kickoff = Some("1:00pm".into());
        info
    }

    fn empty_page() -> EditorPage {
        EditorPage {
            contest_name: "Week 1".into(),
            team_name: "Sunday Funday".into(),
            remaining: 60_000,
            per_open: 6_666,
            filled: 0,
            slots: RosterSlot::iter()
                .map(|slot| EditorSlotRow {
                    slot,
                    label: slot.position_label(),
                    value: None,
                    info: PickInfo::empty(),
                    injury: None,
                })
                .collect(),
            locked: false,
            entry_id: None,
        }
    }

    #[test]
    fn editor_header_offers_save_and_footer_is_in_flow() {
        let mut page = empty_page();
        page.filled = 9;
        page.remaining = 0;
        let html = EditorView {
            page,
            chrome: Chrome::focused("Edit Lineup", "/contests/1"),
            contest_id: 1,
        }
        .render()
        .expect("editor renders");
        assert!(
            html.contains(r#"hx-post="/contests/1/draft-entry/save""#),
            "save action present: {html}"
        );
        assert!(
            html.matches("draft-entry/save").count() == 2,
            "save posts from both header and footer once saveable: {html}"
        );
        assert!(
            !html.contains("sticky bottom-0"),
            "the action footer is in flow, not sticky: {html}"
        );
        assert!(
            html.contains("hx-confirm"),
            "discard asks for confirmation: {html}"
        );
    }

    #[test]
    fn editor_hides_the_header_save_until_saveable() {
        let html = EditorView {
            page: empty_page(),
            chrome: Chrome::focused("Edit Lineup", "/contests/1"),
            contest_id: 1,
        }
        .render()
        .expect("editor renders");
        assert!(
            html.matches("draft-entry/save").count() == 1,
            "only the footer posts save while the draft can't save: {html}"
        );
        assert!(
            html.contains("9 positions left to fill"),
            "the save blocker echoes in the salary band: {html}"
        );
    }

    #[test]
    fn editor_renders_empty() {
        EditorView {
            page: empty_page(),
            chrome: Chrome::focused("Edit Lineup", "/contests/1"),
            contest_id: 1,
        }
        .render()
        .expect("editor renders with no picks");
    }

    #[test]
    fn editor_renders_filled_over_cap_locked() {
        let mut page = empty_page();
        page.remaining = -1_200;
        page.per_open = 0;
        page.filled = 9;
        page.locked = true;
        page.slots = RosterSlot::iter()
            .map(|slot| {
                let value = match slot {
                    RosterSlot::Def => Some(SlotValue::Defense(NflTeamAbbr("KC".into()))),
                    _ => Some(SlotValue::Player(NflPlayerId("00-P".into()))),
                };
                EditorSlotRow {
                    slot,
                    label: slot.position_label(),
                    value,
                    info: player_info(&format!("Player {slot}"), 6_800, Some((15.5, 3))),
                    injury: None,
                }
            })
            .collect();
        EditorView {
            page,
            chrome: Chrome::focused("Edit Lineup", "/contests/1"),
            contest_id: 1,
        }
        .render()
        .expect("editor renders a full locked lineup");
    }

    #[test]
    fn picks_renders_a_player_slot() {
        PickView {
            chrome: Chrome::focused("Select QB", "/contests/1/draft-entry"),
            contest_id: 1,
            slot: RosterSlot::Qb,
            choices: vec![
                SlotChoice {
                    key: SlotValue::Player(NflPlayerId("00-Q".into())),
                    position: "QB".into(),
                    is_picked: true,
                    over_cap: false,
                    injury: None,
                    info: player_info("Quinn Back", 8_000, Some((20.5, 2))),
                },
                SlotChoice {
                    key: SlotValue::Player(NflPlayerId("00-Q2".into())),
                    position: "QB".into(),
                    is_picked: false,
                    over_cap: false,
                    injury: None,
                    info: player_info("Ryan Arm", 7_500, None),
                },
            ],
        }
        .render()
        .expect("picks partial renders");
    }

    #[test]
    fn picks_flag_an_unaffordable_salary_in_red() {
        let html = PickView {
            chrome: Chrome::focused("Select QB", "/contests/1/draft-entry"),
            contest_id: 1,
            slot: RosterSlot::Qb,
            choices: vec![SlotChoice {
                key: SlotValue::Player(NflPlayerId("00-Q".into())),
                position: "QB".into(),
                is_picked: false,
                over_cap: true,
                injury: None,
                info: player_info("Quinn Back", 8_000, Some((20.5, 2))),
            }],
        }
        .render()
        .expect("picks render");
        assert!(
            html.contains(r#"text-error">$8,000"#),
            "unaffordable salary shows red: {html}"
        );
    }

    #[test]
    fn picks_renders_the_def_slot() {
        let mut info = PickInfo::defense(&NflTeamAbbr("KC".into()), None, &[], Some(4_000));
        info.kickoff = Some("1:00pm".into());
        PickView {
            chrome: Chrome::focused("Select DEF", "/contests/1/draft-entry"),
            contest_id: 1,
            slot: RosterSlot::Def,
            choices: vec![SlotChoice {
                key: SlotValue::Defense(NflTeamAbbr("KC".into())),
                position: "D/ST".into(),
                is_picked: true,
                over_cap: false,
                injury: None,
                info,
            }],
        }
        .render()
        .expect("def picks partial renders");
    }

    #[test]
    fn pick_rows_show_the_report_designation() {
        let html = pick_view(
            RosterSlot::Qb,
            vec![
                qb_choice("Adjusted"),
                SlotChoice {
                    injury: Some(InjuryDesignation::Questionable),
                    ..qb_choice("Questionable")
                },
                SlotChoice {
                    injury: Some(InjuryDesignation::InjuredReserve),
                    ..qb_choice("Benched")
                },
            ],
        )
        .render()
        .expect("picks render");
        assert!(
            html.contains(r#"title="Questionable" aria-label="Questionable""#)
                && html.contains(">Q</span>"),
            "the injured carry their short code: {html}"
        );
        assert!(html.contains(">IR</span>"), "IR shows its two-letter code");
        let adjusted = html.find("Adjusted").unwrap();
        let questionable = html.find("Questionable").unwrap();
        let benched = html.find("Benched").unwrap();
        assert!(
            adjusted < questionable && questionable < benched,
            "each chip sits with its own row: {html}"
        );
    }

    #[test]
    fn editor_renders_swap_control_only_on_filled_slots() {
        let mut page = empty_page();
        page.filled = 1;
        for row in &mut page.slots {
            if row.slot == RosterSlot::Qb {
                row.value = Some(SlotValue::Player(NflPlayerId("00-Q".into())));
                row.info = player_info("Quinn Back", 8_000, Some((20.5, 2)));
            }
        }
        let html = EditorView {
            page,
            chrome: Chrome::focused("Edit Lineup", "/contests/1"),
            contest_id: 7,
        }
        .render()
        .expect("editor renders with a filled slot");
        assert!(
            html.contains(r#"aria-label="Swap QB""#),
            "a filled slot gets a swap control: {html}"
        );
        assert!(
            !html.contains(r#"aria-label="Swap RB1""#),
            "an empty slot offers select, not swap: {html}"
        );
        assert!(
            !html.contains("/clear"),
            "clearing lives in the picker, not on the lineup rows: {html}"
        );
        assert!(html.contains("1:00pm"), "kickoff time shown: {html}");
    }

    #[test]
    fn picks_renders_remove_control_only_on_the_current_pick() {
        let html = PickView {
            chrome: Chrome::focused("Select QB", "/contests/1/draft-entry"),
            contest_id: 1,
            slot: RosterSlot::Qb,
            choices: vec![
                SlotChoice {
                    key: SlotValue::Player(NflPlayerId("00-Q".into())),
                    position: "QB".into(),
                    is_picked: true,
                    over_cap: false,
                    injury: None,
                    info: player_info("Quinn Back", 8_000, Some((20.5, 2))),
                },
                SlotChoice {
                    key: SlotValue::Player(NflPlayerId("00-Q2".into())),
                    position: "QB".into(),
                    is_picked: false,
                    over_cap: false,
                    injury: None,
                    info: player_info("Ryan Arm", 7_500, None),
                },
            ],
        }
        .render()
        .expect("picks partial renders");
        assert_eq!(
            html.matches("/contests/1/draft-entry/QB/clear").count(),
            1,
            "exactly the current pick gets a remove control: {html}"
        );
        assert!(
            !html.contains("Selected"),
            "the badge is replaced by a remove control: {html}"
        );
    }

    fn pick_view(slot: RosterSlot, choices: Vec<SlotChoice>) -> PickView {
        PickView {
            chrome: Chrome::focused(
                format!("Select {}", slot.position_label()),
                "/contests/1/draft-entry",
            ),
            contest_id: 1,
            slot,
            choices,
        }
    }

    fn qb_choice(name: &str) -> SlotChoice {
        SlotChoice {
            key: SlotValue::Player(NflPlayerId(format!("00-{name}"))),
            position: "QB".into(),
            is_picked: false,
            over_cap: false,
            injury: None,
            info: player_info(name, 7_500, None),
        }
    }

    #[test]
    fn picks_offer_a_search_that_swaps_the_header() {
        let html = pick_view(RosterSlot::Qb, vec![qb_choice("Quinn Back")])
            .render()
            .expect("picks render");
        assert!(
            html.contains(r#"aria-label="Search""#),
            "the idle header offers a search control: {html}"
        );
        assert!(
            html.contains(r#"placeholder="Search QBs""#),
            "the field names what it searches: {html}"
        );
        assert!(
            html.contains(r#"aria-label="Clear search""#) && html.contains(">Cancel<"),
            "the open search offers both clear and cancel: {html}"
        );
        assert!(
            html.contains(r#"aria-label="Back""#) && html.contains("ph-caret-left"),
            "the picker pops back rather than closing: {html}"
        );
    }

    #[test]
    fn picks_tag_every_row_with_its_lowercased_name() {
        let mut picked = qb_choice("Quinn Back");
        picked.is_picked = true;
        let html = pick_view(RosterSlot::Qb, vec![picked, qb_choice("Ryan Arm")])
            .render()
            .expect("picks render");
        assert!(
            html.contains(r#"data-name="quinn back""#),
            "the current pick filters like any other row: {html}"
        );
        assert!(
            html.contains(r#"data-name="ryan arm""#),
            "unpicked rows carry their name too: {html}"
        );
    }

    #[test]
    fn search_placeholder_reads_as_english_for_every_slot() {
        let phrasing: Vec<String> = RosterSlot::iter()
            .map(|slot| pick_view(slot, Vec::new()).search_placeholder())
            .collect();
        assert_eq!(
            phrasing,
            [
                "Search QBs",
                "Search RBs",
                "Search RBs",
                "Search WRs",
                "Search WRs",
                "Search WRs",
                "Search TEs",
                "Search players",
                "Search defenses",
            ]
        );
    }

    #[test]
    fn parse_slot_accepts_the_enum_strings_and_rejects_garbage() {
        assert_eq!(parse_slot("QB").unwrap(), RosterSlot::Qb);
        assert_eq!(parse_slot("RB2").unwrap(), RosterSlot::Rb2);
        assert_eq!(parse_slot("DEF").unwrap(), RosterSlot::Def);
        assert!(matches!(parse_slot("rb1"), Err(AppError::NotFound)));
        assert!(matches!(parse_slot("WRAP"), Err(AppError::NotFound)));
    }
}
