//! The entry page's data: the structs the template renders and their
//! construction.

use std::collections::HashMap;

use nfl_data::{
    Game, LiveGamePhase, PlayerIdentity, PlayerWeekStats as NflPlayerWeekStats, Season,
    TeamAbbr as NflTeamAbbr,
};

use crate::contests::ContestId;
use crate::contests::published_slate;
use crate::contests::service as contests;
use crate::entries::EntryId;
use crate::entries::store;
use crate::entries::store::SlateSalaries;
use crate::entries::{Lineup, NflPlayerId, RosterSlot};
use crate::fantasy_teams::monogram;
use crate::fantasy_teams::store as fantasy_teams;
use crate::injuries::InjuryDesignation;
use crate::live::{self, ContestScores, Coverage, LiveContestSnapshot};
use crate::media::Media;
use crate::scoring::service as scoring_service;
use crate::scoring::{self, Score, WeekStats};
use crate::{AppError, AppState, User};
use time::OffsetDateTime;

/// Which lineup page a viewer gets.
pub enum EntryView {
    /// Scores: after kickoff, or the locked shell a non-owner sees before it.
    Scored(EntryPage),
    /// The owner's pre-kickoff review: salaries and schedule, no scores.
    Preview(EntryPreview),
}

impl EntryView {
    /// The `/contests/{id}` back target, shared by both pages.
    pub fn contest_id(&self) -> ContestId {
        match self {
            EntryView::Scored(p) => p.contest_id,
            EntryView::Preview(p) => p.contest_id,
        }
    }
}

pub struct EntryPage {
    pub team_name: String,
    pub owner_name: String,
    pub logo_url: Option<String>,
    pub contest_id: ContestId,
    pub contest_name: String,
    pub rank: Option<u32>,
    pub score: ScoreMeta,
    pub picks: Option<Picks>,
}

impl EntryPage {
    /// Up to two initials from the fantasy team name, for the logo fallback.
    pub fn monogram(&self) -> String {
        monogram(&self.team_name)
    }

    /// The entry's total, or `None` while the picks stay hidden.
    pub fn total_str(&self) -> Option<String> {
        self.picks.as_ref().and_then(|p| p.total_str())
    }

    /// The rank as `"3"`, or `None` before results.
    pub fn rank_str(&self) -> Option<String> {
        self.rank.map(|r| r.to_string())
    }
}

pub struct ScoreMeta {
    pub coverage: Coverage,
    pub events_url: Option<String>,
    pub active: bool,
    pub delayed: bool,
    pub official: bool,
}

fn score_meta(
    source: &ContestScores,
    lineup: &Lineup,
    entry_id: EntryId,
    now: OffsetDateTime,
    stream_eligible: bool,
) -> ScoreMeta {
    match source {
        ContestScores::Official(_) => ScoreMeta {
            coverage: Coverage::Complete,
            events_url: None,
            active: false,
            delayed: false,
            official: true,
        },
        ContestScores::Provisional(snapshot) => {
            let total = live::score_live_lineup(lineup, snapshot, now);
            let delayed = snapshot.is_delayed(now);
            ScoreMeta {
                coverage: total.coverage,
                events_url: stream_eligible.then(|| format!("/entries/{}/events", entry_id.0)),
                active: snapshot.has_active_game(),
                delayed,
                official: false,
            }
        }
        ContestScores::Upcoming => ScoreMeta {
            coverage: Coverage::Unavailable,
            events_url: None,
            active: false,
            delayed: false,
            official: false,
        },
        ContestScores::Unavailable => ScoreMeta {
            coverage: Coverage::Unavailable,
            events_url: stream_eligible.then(|| format!("/entries/{}/events", entry_id.0)),
            active: false,
            delayed: true,
            official: false,
        },
    }
}
/// The nine scored lineup rows and their total.
pub struct Picks {
    pub rows: Vec<SlotRow>,
    total: Option<f64>,
}

impl Picks {
    /// The entry's total as `"20.00"`.
    pub fn total_str(&self) -> Option<String> {
        self.total.map(scoring::format_points)
    }
}

/// The game-score line for a row: both teams with their (optional) scores and
/// which side is the pick's team, for bolding.
pub struct GameLine {
    pub away: String,
    pub away_score: Option<i32>,
    pub home: String,
    pub home_score: Option<i32>,
    pub home_is_pick: bool,
}

impl GameLine {
    /// `"BUF 13"`, or `"BUF"` before the game has a score.
    pub fn away_str(&self) -> String {
        side_str(&self.away, self.away_score)
    }
    pub fn home_str(&self) -> String {
        side_str(&self.home, self.home_score)
    }
}

fn side_str(team: &str, score: Option<i32>) -> String {
    match score {
        Some(s) => format!("{team} {s}"),
        None => team.to_string(),
    }
}

/// The slate game the given team plays in, as a display line. `None` when no
/// slate game features the team — the row falls back to its matchup text.
pub fn game_line(slate: &[Game], team: &str) -> Option<GameLine> {
    slate
        .iter()
        .find(|g| g.home_team.0 == team || g.away_team.0 == team)
        .map(|g| GameLine {
            away: g.away_team.0.clone(),
            away_score: g.away_score,
            home: g.home_team.0.clone(),
            home_score: g.home_score,
            home_is_pick: g.home_team.0 == team,
        })
}

/// How a row shows its subject: a player photo, a D/ST team logo, or the
/// silhouette placeholder when a player has no photo.
enum Avatar {
    Headshot(String),
    TeamLogo(String),
    Silhouette,
}

impl Avatar {
    fn url(&self) -> String {
        match self {
            Avatar::Headshot(url) | Avatar::TeamLogo(url) => crate::assets::versioned(url),
            Avatar::Silhouette => crate::assets::versioned(SILHOUETTE),
        }
    }
}

pub(crate) const SILHOUETTE: &str = "/static/img/headshot-placeholder.svg";

/// The salary cap for a nine-slot FanDuel-style lineup.
pub const SALARY_CAP: i64 = 60_000;

/// The pre-kickoff pick block every lineup page renders the same way:
/// identity, this week's matchup and kickoff, price, and season pace. The
/// editor, the pick lists, and the entry preview all embed one.
pub struct PickInfo {
    pub(crate) name: String,
    avatar: Avatar,
    pub(crate) team: Option<String>,
    pub(crate) game: Option<GameLine>,
    /// `"Sun 11/23 1:00pm"` Eastern; `None` while the game has no kickoff time.
    pub(crate) kickoff: Option<String>,
    pub(crate) salary: Option<i64>,
    pub(crate) fppg: Option<f64>,
    pub(crate) played: Option<u32>,
}

impl PickInfo {
    /// A player pick. `pace` is its season-to-date `(FPPG, games played)`.
    pub(crate) fn player(
        name: String,
        headshot: Option<&Media>,
        team: Option<String>,
        slate: &[Game],
        salary: Option<i64>,
        pace: Option<(f64, u32)>,
    ) -> Self {
        PickInfo {
            name,
            avatar: match headshot {
                Some(media) => Avatar::Headshot(media.url()),
                None => Avatar::Silhouette,
            },
            game: team.as_deref().and_then(|t| scoreless_game_line(slate, t)),
            kickoff: team.as_deref().and_then(|t| team_kickoff(slate, t)),
            team,
            salary,
            fppg: pace.map(|(f, _)| f),
            played: pace.map(|(_, n)| n),
        }
    }

    /// A D/ST pick, named `"KC D/ST"` and shown with the team logo.
    pub(crate) fn defense(
        team: &NflTeamAbbr,
        logo: Option<&Media>,
        slate: &[Game],
        salary: Option<i64>,
    ) -> Self {
        PickInfo {
            name: format!("{} D/ST", team.0),
            avatar: Avatar::TeamLogo(
                logo.map_or_else(|| SILHOUETTE.to_string(), |media| media.url()),
            ),
            game: scoreless_game_line(slate, &team.0),
            kickoff: team_kickoff(slate, &team.0),
            team: Some(team.0.clone()),
            salary,
            fppg: None,
            played: None,
        }
    }

    /// An unfilled slot: nameless, silhouette avatar, nothing to show.
    pub(crate) fn empty() -> Self {
        PickInfo {
            name: String::new(),
            avatar: Avatar::Silhouette,
            team: None,
            game: None,
            kickoff: None,
            salary: None,
            fppg: None,
            played: None,
        }
    }

    /// The compact row name; D/ST names return unchanged.
    pub fn abbreviated_name(&self) -> String {
        if self.is_dst() {
            self.name.clone()
        } else {
            abbreviated_name(&self.name)
        }
    }

    /// Whether the name exceeds the content heuristic for a row.
    pub fn is_long_name(&self) -> bool {
        is_long_name(&self.name)
    }

    /// The row's image source.
    pub fn avatar_url(&self) -> String {
        self.avatar.url()
    }

    /// Whether this pick represents a D/ST.
    pub fn is_dst(&self) -> bool {
        self.avatar_is_logo()
    }

    /// A team logo sits contained and padded; a photo fills the cell.
    pub fn avatar_is_logo(&self) -> bool {
        matches!(self.avatar, Avatar::TeamLogo(_))
    }

    pub fn game(&self) -> Option<&GameLine> {
        self.game.as_ref()
    }

    /// The pick's team, the fallback when no slate game features it.
    pub fn team_str(&self) -> String {
        self.team.clone().unwrap_or_else(|| "—".into())
    }

    /// `"$5,700"`, or `"—"` for an unpriced pick.
    pub fn salary_str(&self) -> String {
        match self.salary {
            Some(v) => format!("${}", scoring::format_thousands(v)),
            None => "—".into(),
        }
    }

    /// `"18.4"` for the info column, `"—"` without season history.
    pub fn fppg_str(&self) -> String {
        match self.fppg {
            Some(f) => format!("{f:.1}"),
            None => "—".into(),
        }
    }

    /// `"10"` for the info column, `"—"` without season history.
    pub fn played_str(&self) -> String {
        match self.played {
            Some(n) => n.to_string(),
            None => "—".into(),
        }
    }
}

/// The transient direction of a changed displayed score.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ScoreDirection {
    Increase,
    Decrease,
}

/// One lineup row: a slot label, who filled it, and how they scored.
pub struct SlotRow {
    pub slot: &'static str,
    dom_id: &'static str,
    pub name: String,
    avatar: Avatar,
    team: Option<String>,
    kickoff: Option<String>,
    opponent: Option<String>,
    game: Option<GameLine>,
    /// The collapsed row's headline box score — position-specific for a
    /// player (passing / rushing / receiving), the defensive line for a D/ST.
    top_stats: String,
    score: Score,
    points: Option<f64>,
    score_direction: Option<ScoreDirection>,
    score_delta: Option<f64>,
    pub coverage: Coverage,
    pub phase: Option<LiveGamePhase>,
    pub period: Option<String>,
    pub clock: Option<String>,
    pub stale: bool,
    salary: Option<i64>,
    ownership: Option<f64>,
    injury: Option<InjuryDesignation>,
}

impl SlotRow {
    pub fn dom_id(&self) -> String {
        self.dom_id.to_owned()
    }

    pub fn injury(&self) -> Option<crate::injuries::InjuryChip> {
        self.injury.map(crate::injuries::chip)
    }

    /// The compact row name; D/ST names return unchanged.
    pub fn abbreviated_name(&self) -> String {
        if self.slot == RosterSlot::Def.position_label() {
            self.name.clone()
        } else {
            abbreviated_name(&self.name)
        }
    }

    /// Whether the name exceeds the content heuristic for a row.
    pub fn is_long_name(&self) -> bool {
        is_long_name(&self.name)
    }

    /// The row's image source.
    pub fn avatar_url(&self) -> String {
        self.avatar.url()
    }

    /// A team logo sits contained and padded; a photo fills the cell.
    pub fn avatar_is_logo(&self) -> bool {
        matches!(self.avatar, Avatar::TeamLogo(_))
    }

    pub fn game(&self) -> Option<&GameLine> {
        self.game.as_ref()
    }

    /// `"KC vs BUF"`, `"KC"`, or `"—"` — the fallback when no slate game
    /// features the team.
    pub fn matchup_str(&self) -> String {
        match (&self.team, &self.opponent) {
            (Some(t), Some(o)) => format!("{t} vs {o}"),
            (Some(t), None) => t.clone(),
            (None, _) => "—".into(),
        }
    }

    /// The collapsed row's headline box score.
    pub fn top_stats(&self) -> &str {
        &self.top_stats
    }

    /// The compact fantasy-scoring line.
    pub fn stat_line_str(&self) -> String {
        or_dash(self.score.stat_line())
    }

    /// The per-stat scoring lines, for the expand panel.
    pub fn breakdown(&self) -> &[scoring::ScoreLine] {
        &self.score.lines
    }

    fn points_parts(&self) -> (String, String) {
        let Some(points) = self.points else {
            return (String::new(), String::new());
        };
        let formatted = scoring::format_points(points);
        let (whole, dec) = formatted.rsplit_once('.').expect("format_points is n.nn");
        (whole.to_string(), format!(".{dec}"))
    }

    pub fn points_whole(&self) -> String {
        self.points_parts().0
    }

    pub fn points_dec(&self) -> String {
        self.points_parts().1
    }

    pub fn has_points(&self) -> bool {
        self.points.is_some()
    }

    pub fn score_increased(&self) -> bool {
        self.score_direction == Some(ScoreDirection::Increase)
    }

    pub fn score_decreased(&self) -> bool {
        self.score_direction == Some(ScoreDirection::Decrease)
    }

    /// The flash class a changed score carries on both score spans:
    /// `" score-flash-increase"`, `" score-flash-decrease"`, or `""`.
    pub fn score_flash_class(&self) -> &'static str {
        if self.score_increased() {
            " score-flash-increase"
        } else if self.score_decreased() {
            " score-flash-decrease"
        } else {
            ""
        }
    }

    /// The changed-frame delta under the score: `"+2.00"`, `"-1.75"`, or `""`.
    pub fn score_delta(&self) -> String {
        self.score_delta.map_or_else(String::new, |delta| {
            let formatted = scoring::format_points(delta);
            if formatted.starts_with('-') {
                formatted
            } else {
                format!("+{formatted}")
            }
        })
    }

    pub fn salary_str(&self) -> String {
        match self.salary {
            Some(v) => format!("${}", scoring::format_thousands(v)),
            None => "—".into(),
        }
    }

    pub fn ownership_str(&self) -> String {
        match self.ownership {
            Some(f) => format!("{:.1}%", f * 100.0),
            None => "—".into(),
        }
    }

    pub fn status(&self) -> &str {
        if self.coverage == Coverage::Unavailable {
            "Awaiting stats"
        } else if self.stale {
            "Updates delayed"
        } else if self.phase == Some(LiveGamePhase::Scheduled) {
            self.kickoff.as_deref().unwrap_or("Upcoming")
        } else {
            ""
        }
    }

    pub fn period_clock(&self) -> Option<String> {
        match (&self.period, &self.clock) {
            (Some(period), Some(clock)) => {
                let period = period
                    .parse::<u8>()
                    .map(|period| format!("Q{period}"))
                    .unwrap_or_else(|_| period.clone());
                Some(format!("{period} · {clock}"))
            }
            (Some(period), None) => Some(period.clone()),
            (None, Some(clock)) => Some(clock.clone()),
            (None, None) => None,
        }
    }

    pub fn has_status(&self) -> bool {
        !self.status().is_empty()
    }
}

/// An empty display string reads as a dash, never as a blank cell.
fn or_dash(s: String) -> String {
    if s.is_empty() { "—".into() } else { s }
}

/// Whether a pick shows the passing box score. The player's true position
/// wins, so a FLEX resolves to what the player actually is. An unknown
/// position falls back to the slot.
fn is_passing(slot: RosterSlot, position: Option<&str>) -> bool {
    match position.map(str::to_ascii_uppercase).as_deref() {
        Some("QB") => true,
        Some(_) => false,
        None => slot == RosterSlot::Qb,
    }
}

/// A player's headline box score. A QB shows `C/A, YDS, TD, INT`; every other
/// player shows a combined rushing and receiving line with one total-touchdown
/// count. A zero TD or INT count is dropped, matching the source screens.
fn top_stats_line(slot: RosterSlot, position: Option<&str>, stats: &NflPlayerWeekStats) -> String {
    if is_passing(slot, position) {
        // Weeks ingested before completions and attempts existed report zero
        // for both, so lead with the yards rather than a false "0/0".
        let mut line = if stats.attempts > 0 {
            format!(
                "{}/{}, {} YDS",
                stats.completions, stats.attempts, stats.passing_yards
            )
        } else {
            format!("{} YDS", stats.passing_yards)
        };
        if stats.passing_tds > 0 {
            line.push_str(&format!(", {} TD", stats.passing_tds));
        }
        if stats.passing_interceptions > 0 {
            line.push_str(&format!(", {} INT", stats.passing_interceptions));
        }
        return line;
    }

    let receiving = || format!("{} REC, {} YDS", stats.receptions, stats.receiving_yards);
    let tds = stats.rushing_tds
        + stats.receiving_tds
        + stats.special_teams_tds
        + stats.fumble_recovery_tds;

    let mut parts = Vec::new();
    if stats.rushing_attempts > 0 || stats.rushing_yards != 0 {
        parts.push(format!(
            "{} CAR, {} YDS",
            stats.rushing_attempts, stats.rushing_yards
        ));
    }
    if stats.receptions > 0 || stats.receiving_yards != 0 {
        parts.push(receiving());
    }
    if tds > 0 {
        parts.push(format!("{tds} TD"));
    }
    if parts.is_empty() {
        // Played, but recorded no rushing or receiving production.
        parts.push(receiving());
    }
    parts.join(", ")
}

/// Score the lineup into the nine display rows, in roster order. `names` is
/// keyed by gsis id; a pick in neither nfl-data source is named by its raw
/// id. The named team is the matchup fallback for a slot with no stat row
/// that week.
#[allow(clippy::too_many_arguments)]
pub fn build_picks(
    lineup: &Lineup,
    names: &HashMap<String, PlayerIdentity>,
    stats: &WeekStats,
    slate: &[Game],
    salaries: &SlateSalaries,
    ownership: &Ownership,
    headshots: &HashMap<String, Media>,
    defense_logo: Option<&Media>,
    injuries: &HashMap<NflPlayerId, InjuryDesignation>,
) -> Picks {
    let mut rows: Vec<SlotRow> = lineup
        .player_slots()
        .into_iter()
        .map(|(slot, id)| {
            let known = names.get(id.as_ref());
            let stat = stats.players.get(id);
            let team = stat
                .map(|s| s.team.0.clone())
                .or_else(|| known.and_then(|k| k.team.as_ref().map(|t| t.0.clone())));
            let avatar = match headshots.get(id.as_ref()) {
                Some(media) => Avatar::Headshot(media.url()),
                None => Avatar::Silhouette,
            };
            SlotRow {
                slot: slot.position_label(),
                dom_id: slot.dom_id(),
                name: known.map_or_else(|| id.as_ref().to_string(), |k| k.name.clone()),
                avatar,
                team: team.clone(),
                kickoff: team.as_deref().and_then(|team| team_kickoff(slate, team)),
                opponent: stat.and_then(|s| s.opponent.as_ref().map(|o| o.0.clone())),
                game: team.as_deref().and_then(|t| game_line(slate, t)),
                top_stats: stat.map_or_else(String::new, |s| {
                    top_stats_line(slot, known.and_then(|k| k.position.as_deref()), s)
                }),
                score: stat.map(scoring::score_player).unwrap_or_default(),
                points: Some(stat.map_or(0.0, |stat| scoring::score_player(stat).total)),
                score_direction: None,
                score_delta: None,
                coverage: Coverage::Complete,
                phase: None,
                period: None,
                clock: None,
                stale: false,
                salary: salaries.players.get(id).copied(),
                ownership: ownership.player(id),
                injury: injuries.get(id).copied(),
            }
        })
        .collect();

    let defense = stats.defenses.get(&lineup.def);
    let def_team = lineup.def.0.clone();
    let def_score = defense.map(scoring::score_defense).unwrap_or_default();
    let def_points = def_score.total;
    rows.push(SlotRow {
        slot: RosterSlot::Def.position_label(),
        dom_id: RosterSlot::Def.dom_id(),
        name: format!("{def_team} D/ST"),
        avatar: Avatar::TeamLogo(
            defense_logo.map_or_else(|| SILHOUETTE.to_string(), |media| media.url()),
        ),
        team: Some(def_team.clone()),
        kickoff: team_kickoff(slate, &def_team),
        opponent: defense.map(|t| t.opponent.0.clone()),
        game: game_line(slate, &def_team),
        // A defense has no box score; its collapsed line is its scoring line.
        // With no defense stat row there is nothing to show, not even a dash.
        top_stats: defense.map_or_else(String::new, |_| or_dash(def_score.stat_line())),
        score: def_score,
        points: Some(def_points),
        score_direction: None,
        score_delta: None,
        coverage: Coverage::Complete,
        phase: None,
        period: None,
        clock: None,
        stale: false,
        salary: salaries.defenses.get(&lineup.def).copied(),
        ownership: ownership.defense(&lineup.def),
        injury: None,
    });

    let total = Some(rows.iter().map(|r| r.points.unwrap_or_default()).sum());
    Picks { rows, total }
}

pub(crate) fn apply_live_snapshot(
    picks: &mut Picks,
    lineup: &Lineup,
    snapshot: &LiveContestSnapshot,
    now: OffsetDateTime,
) {
    for ((slot, _), row) in lineup.player_slots().into_iter().zip(picks.rows.iter_mut()) {
        apply_live_row(row, slot, lineup, snapshot, now);
    }
    if let Some(row) = picks.rows.last_mut() {
        apply_live_row(row, RosterSlot::Def, lineup, snapshot, now);
    }
    picks.total = live::score_live_lineup(lineup, snapshot, now).points;
}

fn apply_live_row(
    row: &mut SlotRow,
    slot: RosterSlot,
    lineup: &Lineup,
    snapshot: &LiveContestSnapshot,
    now: OffsetDateTime,
) {
    let status = live::score_live_slot(lineup, slot, snapshot, now);
    row.points = if status.phase == Some(LiveGamePhase::Scheduled) {
        None
    } else {
        status.total.points
    };
    row.coverage = status.total.coverage;
    row.phase = status.phase;
    row.period = status.period;
    row.clock = status.clock;
    row.stale = status.stale;
    if status.total.points.is_none() {
        row.score = Score::default();
        row.top_stats = "Awaiting stats".into();
    } else if status.phase == Some(LiveGamePhase::Scheduled) {
        row.score = Score::default();
        row.top_stats = String::new();
    }
}

pub(crate) fn update_score_directions(
    page: &mut EntryPage,
    previous: &mut HashMap<&'static str, f64>,
) {
    let Some(picks) = page.picks.as_mut() else {
        previous.clear();
        return;
    };

    for row in &mut picks.rows {
        let outcome = match row.points {
            Some(current) => previous.insert(row.dom_id, current).and_then(|old| {
                score_direction(old, current).map(|direction| (direction, current - old))
            }),
            None => {
                previous.remove(row.dom_id);
                None
            }
        };
        (row.score_direction, row.score_delta) = outcome
            .map_or((None, None), |(direction, delta)| {
                (Some(direction), Some(delta))
            });
    }
}

fn score_direction(previous: f64, current: f64) -> Option<ScoreDirection> {
    if scoring::ties(previous, current) {
        None
    } else if current > previous {
        Some(ScoreDirection::Increase)
    } else {
        Some(ScoreDirection::Decrease)
    }
}

fn clear_live_scores(picks: &mut Picks) {
    for row in &mut picks.rows {
        row.score = Score::default();
        row.points = None;
        row.score_direction = None;
        row.score_delta = None;
        row.coverage = Coverage::Unavailable;
        row.phase = None;
        row.period = None;
        row.clock = None;
        row.stale = false;
        row.top_stats = "Awaiting stats".into();
    }
    picks.total = None;
}

/// The owner's pre-kickoff lineup review: the scored page's skeleton carrying
/// what matters before play — salaries and the slate schedule.
pub struct EntryPreview {
    pub team_name: String,
    pub owner_name: String,
    pub logo_url: Option<String>,
    pub contest_id: ContestId,
    /// Names the page: "Week 12 Lineup".
    pub contest_name: String,
    pub rows: Vec<PreviewRow>,
    spent: i64,
}

impl EntryPreview {
    /// Up to two initials from the fantasy team name, for the logo fallback.
    pub fn monogram(&self) -> String {
        monogram(&self.team_name)
    }

    /// What the lineup left under the cap, `"$600"`.
    pub fn remaining_str(&self) -> String {
        format!("${}", scoring::format_thousands(SALARY_CAP - self.spent))
    }
}

/// One preview row: the slot and who fills it.
pub struct PreviewRow {
    pub slot: &'static str,
    pub info: PickInfo,
}

/// Preview rows in roster order, plus the total salary spent. Game scores are
/// stripped: nothing has been played from this entry's point of view.
pub fn build_preview(
    lineup: &Lineup,
    names: &HashMap<String, PlayerIdentity>,
    slate: &[Game],
    salaries: &SlateSalaries,
    averages: &HashMap<String, (f64, u32)>,
    headshots: &HashMap<String, Media>,
    defense_logo: Option<&Media>,
) -> (Vec<PreviewRow>, i64) {
    let mut rows: Vec<PreviewRow> = lineup
        .player_slots()
        .into_iter()
        .map(|(slot, id)| {
            let known = names.get(id.as_ref());
            PreviewRow {
                slot: slot.position_label(),
                info: PickInfo::player(
                    known.map_or_else(|| id.as_ref().to_string(), |k| k.name.clone()),
                    headshots.get(id.as_ref()),
                    known.and_then(|k| k.team.as_ref().map(|t| t.0.clone())),
                    slate,
                    salaries.players.get(id).copied(),
                    averages.get(id.as_ref()).copied(),
                ),
            }
        })
        .collect();

    rows.push(PreviewRow {
        slot: RosterSlot::Def.position_label(),
        info: PickInfo::defense(
            &lineup.def,
            defense_logo,
            slate,
            salaries.defenses.get(&lineup.def).copied(),
        ),
    });

    let spent = rows.iter().filter_map(|r| r.info.salary).sum();
    (rows, spent)
}

/// A content heuristic for long names; CSS handles narrow containers.
pub(crate) const LONG_NAME_CHARS: usize = 15;

/// A player name with its first word abbreviated when it has a surname.
/// Already-initialized first tokens pass through unchanged. Two-character first
/// names are also kept intact. Everything after the first word is kept, so
/// compound last names survive; a single-word name passes through.
pub(crate) fn abbreviated_name(name: &str) -> String {
    match name.split_once(' ') {
        Some((first, _)) if first.contains('.') || first.chars().count() == 2 => name.to_string(),
        Some((first, rest)) if !rest.is_empty() => match first.chars().next() {
            Some(initial) => format!("{initial}. {rest}"),
            None => name.to_string(),
        },
        _ => name.to_string(),
    }
}

/// Whether a name exceeds the content-length heuristic for a row.
pub(crate) fn is_long_name(name: &str) -> bool {
    name.chars().count() > LONG_NAME_CHARS
}

/// `game_line` with any scores stripped, for pre-kickoff pages: replayed dev
/// data may already carry them, but nothing has been played yet.
pub(crate) fn scoreless_game_line(slate: &[Game], team: &str) -> Option<GameLine> {
    game_line(slate, team).map(|mut g| {
        g.away_score = None;
        g.home_score = None;
        g
    })
}

/// The team's slate kickoff as `"Sun 11/23 1:00pm"` Eastern, `None` while
/// unscheduled.
pub(crate) fn team_kickoff(slate: &[Game], team: &str) -> Option<String> {
    slate
        .iter()
        .find(|g| g.home_team.0 == team || g.away_team.0 == team)
        .and_then(|g| g.kickoff_eastern())
        .map(short_kickoff)
}

/// `"Sun 11/23 1:00pm"`, in the offset `et` carries (callers pass Eastern).
fn short_kickoff(et: OffsetDateTime) -> String {
    use time::macros::format_description;
    et.format(format_description!(
        "[weekday repr:short] [month padding:none]/[day padding:none] [hour repr:12 padding:none]:[minute][period case:lower]"
    ))
    .unwrap_or_default()
}

/// Roster ownership across a contest's lineups: the fraction of lineups that
/// include each player and each team defense.
#[derive(Default)]
pub struct Ownership {
    players: HashMap<NflPlayerId, f64>,
    defenses: HashMap<NflTeamAbbr, f64>,
}

impl Ownership {
    pub fn player(&self, id: &NflPlayerId) -> Option<f64> {
        self.players.get(id).copied()
    }
    pub fn defense(&self, team: &NflTeamAbbr) -> Option<f64> {
        self.defenses.get(team).copied()
    }
}

/// Count each pick across all lineups and divide by the lineup count.
pub fn compute_ownership<'a>(lineups: impl IntoIterator<Item = &'a Lineup>) -> Ownership {
    let mut total = 0.0;
    let mut player_counts: HashMap<NflPlayerId, u32> = HashMap::new();
    let mut def_counts: HashMap<NflTeamAbbr, u32> = HashMap::new();
    for lineup in lineups {
        total += 1.0;
        for id in lineup.player_ids() {
            *player_counts.entry(id.clone()).or_insert(0) += 1;
        }
        *def_counts.entry(lineup.def.clone()).or_insert(0) += 1;
    }
    let frac = |c: u32| c as f64 / total;
    Ownership {
        players: player_counts
            .into_iter()
            .map(|(k, c)| (k, frac(c)))
            .collect(),
        defenses: def_counts.into_iter().map(|(k, c)| (k, frac(c))).collect(),
    }
}
pub async fn page(state: &AppState, viewer: &User, id: EntryId) -> Result<EntryView, AppError> {
    let reader = state.db.reader();
    let entry = store::by_id(reader, id).await?.ok_or(AppError::NotFound)?;
    fantasy_teams::team_for_user(reader, entry.league_id, viewer.id)
        .await?
        .ok_or(AppError::NotFound)?;

    let season = Season(state.config.season);
    let season_games = state.nfl.games(season).await?;
    let slate = published_slate(reader, entry.contest.id, &season_games).await?;
    let week = contests::slate_week(&slate).ok_or(AppError::NotFound)?;
    let now = state.now();

    let is_owner = entry.team.user_id == viewer.id;
    let kicked_off = contests::slate_locked(&slate, now);

    if is_owner && !kicked_off {
        let ids = entry.lineup.player_ids();
        let player_ids: Vec<&str> = ids.iter().map(|id| id.as_ref()).collect();
        let names = state.nfl.identify(&ids).await?;
        let avatars =
            crate::avatars::for_lineup(&state.db, &player_ids, &[entry.lineup.def.0.as_str()])
                .await?;
        let salaries = store::slate_salaries(reader, entry.contest.id).await?;
        let averages = scoring_service::season_averages(&state.nfl, season, &ids, week).await?;
        let (rows, spent) = build_preview(
            &entry.lineup,
            &names,
            &slate,
            &salaries,
            &averages,
            &avatars.players,
            avatars.teams.get(entry.lineup.def.0.as_str()),
        );
        return Ok(EntryView::Preview(EntryPreview {
            team_name: entry.team.name,
            owner_name: entry.team.owner_name,
            logo_url: entry.team.logo.map(|m| m.url()),
            contest_id: entry.contest.id,
            contest_name: entry.contest.name,
            rows,
            spent,
        }));
    }

    let (picks, rank, score) = if kicked_off {
        let ids = entry.lineup.player_ids();
        let player_ids: Vec<&str> = ids.iter().map(|id| id.as_ref()).collect();
        let source = live::load_contest_scores(state, entry.contest.id).await?;
        let stream_eligible = live::stream_eligible(state, entry.contest.id, &source).await?;
        let unsynced = WeekStats::default();
        let (week_stats, display_slate) = match &source {
            ContestScores::Official(stats) => (stats.as_ref(), slate.clone()),
            ContestScores::Provisional(snapshot) => (&snapshot.stats, snapshot.scored_games()),
            ContestScores::Upcoming | ContestScores::Unavailable => (&unsynced, slate.clone()),
        };
        let names = state.nfl.identify(&ids).await?;
        let avatars =
            crate::avatars::for_lineup(&state.db, &player_ids, &[entry.lineup.def.0.as_str()])
                .await?;
        let contest_entries = store::by_contest(reader, entry.contest.id, entry.league_id).await?;
        let salaries = store::slate_salaries(reader, entry.contest.id).await?;

        let ownership = compute_ownership(contest_entries.iter().map(|e| &e.lineup));

        let rank = match &source {
            ContestScores::Official(stats) => {
                contests::rank_contest_entries(contests::entrants(&contest_entries, Some(stats)))
                    .into_iter()
                    .find(|row| row.id == id)
                    .and_then(|row| row.rank)
            }
            ContestScores::Provisional(snapshot) => {
                contests::rank_live_entries(&contest_entries, snapshot, now)
                    .into_iter()
                    .find(|row| row.id == id)
                    .and_then(|row| row.rank)
            }
            ContestScores::Upcoming | ContestScores::Unavailable => None,
        };

        let mut picks = build_picks(
            &entry.lineup,
            &names,
            week_stats,
            &display_slate,
            &salaries,
            &ownership,
            &avatars.players,
            avatars.teams.get(entry.lineup.def.0.as_str()),
            &live::slate_tuple(&slate)
                .and_then(|key| state.injuries.report(&key))
                .map(|report| report.by_gsis)
                .unwrap_or_default(),
        );
        match &source {
            ContestScores::Provisional(snapshot) => {
                apply_live_snapshot(&mut picks, &entry.lineup, snapshot, now);
            }
            ContestScores::Unavailable => clear_live_scores(&mut picks),
            ContestScores::Official(_) | ContestScores::Upcoming => {}
        }
        let score = score_meta(&source, &entry.lineup, id, now, stream_eligible);
        for row in &mut picks.rows {
            if score.official || row.phase == Some(LiveGamePhase::Final) {
                row.injury = None;
            }
        }
        (Some(picks), rank, score)
    } else {
        (
            None,
            None,
            ScoreMeta {
                coverage: Coverage::Unavailable,
                events_url: None,
                active: false,
                delayed: false,
                official: false,
            },
        )
    };

    Ok(EntryView::Scored(EntryPage {
        team_name: entry.team.name,
        owner_name: entry.team.owner_name,
        logo_url: entry.team.logo.map(|m| m.url()),
        contest_id: entry.contest.id,
        contest_name: entry.contest.name,
        rank,
        score,
        picks,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::factories;
    use nfl_data::{
        Game, PlayerWeekStats as NflPlayerWeekStats, TeamAbbr as NflTeamAbbr,
        TeamWeekStats as NflTeamWeekStats,
    };

    fn lineup() -> Lineup {
        Lineup {
            qb: NflPlayerId("00-QB".into()),
            rb1: NflPlayerId("00-RB1".into()),
            rb2: NflPlayerId("00-RB2".into()),
            wr1: NflPlayerId("00-WR1".into()),
            wr2: NflPlayerId("00-WR2".into()),
            wr3: NflPlayerId("00-WR3".into()),
            te: NflPlayerId("00-TE".into()),
            flex: NflPlayerId("00-FLEX".into()),
            def: NflTeamAbbr("KC".into()),
        }
    }

    /// Every pick named, each on KC.
    fn names() -> HashMap<String, PlayerIdentity> {
        [
            ("00-QB", "Quinn Back"),
            ("00-RB1", "Alpha Runner"),
            ("00-RB2", "Beta Runner"),
            ("00-WR1", "Wide One"),
            ("00-WR2", "Wide Two"),
            ("00-WR3", "Wide Three"),
            ("00-TE", "Tight End"),
            ("00-FLEX", "Flex Player"),
        ]
        .into_iter()
        .map(|(gsis, name)| (gsis.to_string(), factories::identity(name, Some("KC"))))
        .collect()
    }

    /// RB1 with 100 rushing yards (13.00) and the KC defense allowing 3
    /// (7.00); every other slot has no stat row.
    fn week_stats() -> WeekStats {
        let rb = NflPlayerWeekStats {
            opponent: Some(NflTeamAbbr("BUF".into())),
            ..factories::rb_stats("00-RB1", 1, 100)
        };
        let def = NflTeamWeekStats {
            points_allowed: 3,
            ..factories::defense_stats("KC", "LA", 1)
        };
        WeekStats {
            players: [(NflPlayerId("00-RB1".into()), rb)].into(),
            defenses: [(NflTeamAbbr("KC".into()), def)].into(),
        }
    }

    /// RB1's game (KC home vs BUF) with a final score.
    fn slate() -> Vec<Game> {
        vec![Game {
            home_score: Some(20),
            away_score: Some(13),
            ..factories::game("2025_01_BUF_KC", 1)
        }]
    }

    /// The default page inputs, with each part overridable by a test.
    struct Fixture {
        names: HashMap<String, PlayerIdentity>,
        stats: WeekStats,
        salaries: SlateSalaries,
        ownership: Ownership,
    }

    impl Default for Fixture {
        fn default() -> Fixture {
            Fixture {
                names: names(),
                stats: week_stats(),
                salaries: SlateSalaries {
                    players: HashMap::new(),
                    defenses: HashMap::new(),
                },
                ownership: compute_ownership(&[lineup()]),
            }
        }
    }

    impl Fixture {
        fn picks(&self) -> Picks {
            self.picks_with(&HashMap::new(), None)
        }

        fn picks_with(
            &self,
            headshots: &HashMap<String, Media>,
            defense_logo: Option<&Media>,
        ) -> Picks {
            build_picks(
                &lineup(),
                &self.names,
                &self.stats,
                &slate(),
                &self.salaries,
                &self.ownership,
                headshots,
                defense_logo,
                &HashMap::new(),
            )
        }
    }

    /// The fixture's rows, for the tests that override nothing.
    fn picks() -> Picks {
        Fixture::default().picks()
    }

    fn page_with_picks(picks: Option<Picks>) -> EntryPage {
        EntryPage {
            team_name: "Test Team".into(),
            owner_name: "Test Owner".into(),
            logo_url: None,
            contest_id: ContestId(1),
            contest_name: "Test Contest".into(),
            rank: None,
            score: ScoreMeta {
                coverage: Coverage::Complete,
                events_url: None,
                active: false,
                delayed: false,
                official: false,
            },
            picks,
        }
    }

    #[test]
    fn scheduled_row_shows_kickoff_and_blank_score() {
        let mut picks = picks();
        let row = &mut picks.rows[0];
        row.kickoff = Some("Sun 9/7 1:00pm".into());
        row.coverage = Coverage::Complete;
        row.phase = Some(LiveGamePhase::Scheduled);
        row.points = None;

        assert_eq!(row.status(), "Sun 9/7 1:00pm");
        assert!(row.has_status());
        assert_eq!(row.points_whole(), "");
        assert_eq!(row.points_dec(), "");
        assert!(!row.has_points());

        row.kickoff = None;
        assert_eq!(row.status(), "Upcoming");

        row.points = Some(0.0);
        row.phase = Some(LiveGamePhase::Final);
        assert!(row.has_points());
        assert_eq!(row.points_whole(), "0");
        assert_eq!(row.points_dec(), ".00");
    }

    #[test]
    fn score_direction_uses_display_precision() {
        assert!(matches!(
            score_direction(10.00, 11.00),
            Some(ScoreDirection::Increase)
        ));
        assert!(matches!(
            score_direction(11.00, 10.00),
            Some(ScoreDirection::Decrease)
        ));
        assert!(score_direction(10.00, 10.00).is_none());
        assert!(score_direction(10.00, 10.004).is_none());
        assert!(score_direction(10.004, 10.00).is_none());
    }

    #[test]
    fn score_delta_formats_signs_rounding_and_dash() {
        let mut picks = picks();
        let row = &mut picks.rows[0];
        assert_eq!(row.score_delta(), "");
        row.score_delta = Some(2.0);
        assert_eq!(row.score_delta(), "+2.00");
        row.score_delta = Some(-1.75);
        assert_eq!(row.score_delta(), "-1.75");
        row.score_delta = Some(1.236);
        assert_eq!(row.score_delta(), "+1.24");
    }

    #[test]
    fn update_score_directions_tracks_numeric_rows_and_resets_missing_rows() {
        let mut page = page_with_picks(Some(picks()));
        let mut previous = HashMap::new();

        update_score_directions(&mut page, &mut previous);
        assert!(
            page.picks
                .as_ref()
                .unwrap()
                .rows
                .iter()
                .all(|row| !row.score_increased() && !row.score_decreased())
        );

        let picks = page.picks.as_mut().unwrap();
        picks.rows[1].points = Some(14.0);
        picks.rows[2].points = Some(0.004);
        update_score_directions(&mut page, &mut previous);
        let picks = page.picks.as_ref().unwrap();
        assert!(picks.rows[1].score_increased());
        assert_eq!(picks.rows[1].score_delta(), "+1.00");
        assert!(!picks.rows[2].score_increased());
        assert!(!picks.rows[2].score_decreased());
        assert_eq!(picks.rows[2].score_delta(), "");

        page.picks.as_mut().unwrap().rows[1].points = Some(14.004);
        update_score_directions(&mut page, &mut previous);
        assert!(!page.picks.as_ref().unwrap().rows[1].score_increased());
        assert_eq!(page.picks.as_ref().unwrap().rows[1].score_delta(), "");

        page.picks.as_mut().unwrap().rows[1].points = None;
        update_score_directions(&mut page, &mut previous);
        assert!(!previous.contains_key("rb1"));
        assert!(!page.picks.as_ref().unwrap().rows[1].score_increased());
        assert_eq!(page.picks.as_ref().unwrap().rows[1].score_delta(), "");

        page.picks.as_mut().unwrap().rows[1].points = Some(15.0);
        update_score_directions(&mut page, &mut previous);
        assert!(!page.picks.as_ref().unwrap().rows[1].score_increased());
        assert_eq!(page.picks.as_ref().unwrap().rows[1].score_delta(), "");

        page.picks = None;
        update_score_directions(&mut page, &mut previous);
        assert!(previous.is_empty());
    }

    #[test]
    fn rows_come_in_roster_order_with_slot_labels() {
        let picks = picks();
        let labels: Vec<&str> = picks.rows.iter().map(|r| r.slot).collect();
        assert_eq!(
            labels,
            ["QB", "RB", "RB", "WR", "WR", "WR", "TE", "FLEX", "DEF"]
        );
        assert_eq!(picks.rows[0].name, "Quinn Back");
        assert_eq!(picks.rows[1].name, "Alpha Runner");
        assert_eq!(picks.rows[8].name, "KC D/ST");
    }

    #[test]
    fn rows_have_unique_dom_ids_for_live_updates() {
        let picks = picks();
        let ids: Vec<String> = picks.rows.iter().map(SlotRow::dom_id).collect();
        assert_eq!(
            ids,
            ["qb", "rb1", "rb2", "wr1", "wr2", "wr3", "te", "flex", "def"]
        );
    }

    #[test]
    fn scored_row_keeps_full_breakdown_and_splits_points() {
        let picks = picks();
        let rb1 = &picks.rows[1];
        assert_eq!(rb1.stat_line_str(), "100 RuY, 100+ RuY Gm");
        assert_eq!(rb1.points_whole(), "13");
        assert_eq!(rb1.points_dec(), ".00");
        let labels: Vec<String> = rb1.breakdown().iter().map(|l| l.label()).collect();
        assert!(labels.contains(&"100 RuY".to_string()));
    }

    #[test]
    fn player_row_uses_local_media_then_silhouette() {
        let f = Fixture::default();
        let media = Media::fixture("headshot.png", time::macros::datetime!(2026-08-09 12:00:00));
        let headshots = HashMap::from([("00-QB".to_string(), media.clone())]);
        let picks = f.picks_with(&headshots, None);
        assert_eq!(picks.rows[0].avatar_url(), media.url());
        assert!(!picks.rows[0].avatar_is_logo());
        assert_eq!(
            picks.rows[3].avatar_url(),
            crate::assets::versioned(SILHOUETTE)
        );
    }

    #[test]
    fn defense_row_uses_silhouette_when_logo_missing() {
        let picks = picks();
        let def = &picks.rows[8];
        assert!(def.avatar_is_logo());
        assert_eq!(def.avatar_url(), crate::assets::versioned(SILHOUETTE));
    }

    #[test]
    fn row_carries_game_line_and_salary_and_ownership() {
        let mut f = Fixture::default();
        f.salaries
            .players
            .insert(NflPlayerId("00-RB1".into()), 5700);
        let picks = f.picks();
        let rb1 = &picks.rows[1];
        let g = rb1.game().expect("game line");
        assert_eq!(g.home, "KC");
        assert_eq!(g.home_score, Some(20));
        assert_eq!(rb1.salary_str(), "$5,700");
        assert_eq!(rb1.ownership_str(), "100.0%");
    }

    #[test]
    fn missing_salary_and_ownership_render_a_dash() {
        let picks = Fixture {
            ownership: Ownership::default(),
            ..Fixture::default()
        }
        .picks();
        assert_eq!(picks.rows[0].salary_str(), "—");
        assert_eq!(picks.rows[0].ownership_str(), "—");
    }

    #[test]
    fn top_stats_line_is_position_specific() {
        use RosterSlot::*;
        let qb = NflPlayerWeekStats {
            completions: 26,
            attempts: 38,
            passing_yards: 182,
            passing_tds: 1,
            passing_interceptions: 1,
            ..factories::player_stats("00-QB", 1)
        };
        assert_eq!(
            top_stats_line(Qb, Some("QB"), &qb),
            "26/38, 182 YDS, 1 TD, 1 INT"
        );

        let rb = NflPlayerWeekStats {
            rushing_attempts: 11,
            rushing_yards: 105,
            rushing_tds: 2,
            ..factories::player_stats("00-RB", 1)
        };
        assert_eq!(
            top_stats_line(Rb1, Some("RB"), &rb),
            "11 CAR, 105 YDS, 2 TD"
        );

        // A pass-catching back shows both lines with one combined TD count.
        let dual = NflPlayerWeekStats {
            rushing_attempts: 11,
            rushing_yards: 105,
            rushing_tds: 1,
            receptions: 3,
            receiving_yards: 25,
            receiving_tds: 1,
            ..factories::player_stats("00-RB", 1)
        };
        assert_eq!(
            top_stats_line(Rb1, Some("RB"), &dual),
            "11 CAR, 105 YDS, 3 REC, 25 YDS, 2 TD"
        );

        // A zero TD count is dropped.
        let rb_no_td = NflPlayerWeekStats {
            rushing_attempts: 14,
            rushing_yards: 43,
            ..factories::player_stats("00-RB", 1)
        };
        assert_eq!(top_stats_line(Rb2, Some("RB"), &rb_no_td), "14 CAR, 43 YDS");

        let wr = NflPlayerWeekStats {
            receptions: 2,
            receiving_yards: 37,
            ..factories::player_stats("00-WR", 1)
        };
        assert_eq!(top_stats_line(Wr1, Some("WR"), &wr), "2 REC, 37 YDS");

        let te = NflPlayerWeekStats {
            receptions: 5,
            receiving_yards: 41,
            receiving_tds: 1,
            ..factories::player_stats("00-TE", 1)
        };
        assert_eq!(top_stats_line(Te, Some("TE"), &te), "5 REC, 41 YDS, 1 TD");

        // Played but no production — the receiving line still shows.
        let blank = NflPlayerWeekStats {
            ..factories::player_stats("00-WR", 1)
        };
        assert_eq!(top_stats_line(Wr1, Some("WR"), &blank), "0 REC, 0 YDS");

        // FLEX resolves to the player's true position (a receiver here).
        assert_eq!(top_stats_line(Flex, Some("WR"), &wr), "2 REC, 37 YDS");
        // Unknown position at FLEX is never a QB — the skill line.
        assert_eq!(top_stats_line(Flex, None, &wr), "2 REC, 37 YDS");
        // Unknown position at the QB slot falls back to the passing line.
        assert_eq!(top_stats_line(Qb, None, &qb), "26/38, 182 YDS, 1 TD, 1 INT");

        // A week ingested before completions and attempts existed: no "0/0".
        let old_week = NflPlayerWeekStats {
            passing_yards: 284,
            passing_tds: 1,
            ..factories::player_stats("00-QB", 1)
        };
        assert_eq!(top_stats_line(Qb, Some("QB"), &old_week), "284 YDS, 1 TD");
    }

    #[test]
    fn row_top_stats_is_the_box_score_not_the_scoring_line() {
        let picks = picks();
        // RB1's collapsed line is the natural box score (the fixture sets no
        // carries), distinct from its fantasy-scoring line.
        assert_eq!(picks.rows[1].top_stats(), "0 CAR, 100 YDS");
        assert_eq!(picks.rows[1].stat_line_str(), "100 RuY, 100+ RuY Gm");
        assert_eq!(picks.rows[0].top_stats(), "");
    }

    #[test]
    fn statless_row_blanks_the_stats_cell() {
        let picks = picks();
        let qb = &picks.rows[0];
        assert_eq!(qb.top_stats(), "");
        assert_eq!(qb.stat_line_str(), "—");
        assert_eq!(qb.points_whole(), "0");
        assert_eq!(qb.points_dec(), ".00");
        assert_eq!(qb.matchup_str(), "KC");
    }

    #[test]
    fn defense_row_scores_points_allowed_tier() {
        let picks = picks();
        let def = &picks.rows[8];
        assert_eq!(def.stat_line_str(), "3 DE/PA");
        assert_eq!(def.matchup_str(), "KC vs LA");
        assert_eq!(def.points_whole(), "7");
        assert_eq!(def.points_dec(), ".00");
    }

    #[test]
    fn statless_defense_row_when_defense_has_no_stats() {
        let mut f = Fixture::default();
        f.stats.defenses.clear();
        let picks = f.picks();
        let def = &picks.rows[8];
        assert_eq!(def.top_stats(), "");
        assert_eq!(def.stat_line_str(), "—");
        assert_eq!(def.points_whole(), "0");
        assert_eq!(def.points_dec(), ".00");
        assert_eq!(def.matchup_str(), "KC");
    }

    #[test]
    fn scoreless_stat_row_still_renders_a_dash() {
        // A defense that allows 21-27 points and does nothing else plays, so
        // it has a stat row and a matchup, but nothing that scores. The stat
        // cell must not go blank.
        let scoreless = NflTeamWeekStats {
            points_allowed: 24,
            ..factories::defense_stats("KC", "LA", 1)
        };
        let mut f = Fixture::default();
        f.stats.defenses = [(NflTeamAbbr("KC".into()), scoreless)].into();
        let picks = f.picks();
        let def = &picks.rows[8];
        assert_eq!(def.matchup_str(), "KC vs LA");
        assert_eq!(def.top_stats(), "—");
        assert_eq!(def.stat_line_str(), "—");
    }

    #[test]
    fn matchup_renders_dash_with_no_team_and_no_stat_row() {
        let mut f = Fixture::default();
        f.names
            .insert("00-QB".into(), factories::identity("Quinn Back", None));
        let picks = f.picks();
        assert_eq!(picks.rows[0].matchup_str(), "—");
    }

    #[test]
    fn total_sums_all_nine_slots() {
        let picks = picks();
        assert_eq!(picks.total_str().as_deref(), Some("20.00"));
    }

    #[test]
    fn abbreviated_name_keeps_everything_after_the_first_word() {
        assert_eq!(abbreviated_name("Drake Maye"), "D. Maye");
        assert_eq!(abbreviated_name("Patrick Mahomes"), "P. Mahomes");
        assert_eq!(abbreviated_name("Jameson Williams"), "J. Williams");
        assert_eq!(abbreviated_name("Wan'Dale Robinson"), "W. Robinson");
        assert_eq!(abbreviated_name("Jaxon Smith-Njigba"), "J. Smith-Njigba");
        assert_eq!(
            abbreviated_name("Amon-Ra St. Brown Jr."),
            "A. St. Brown Jr."
        );
        assert_eq!(
            abbreviated_name("Mononymous-Longer-Than-Fits"),
            "Mononymous-Longer-Than-Fits"
        );
    }

    #[test]
    fn abbreviated_name_preserves_initialed_first_tokens() {
        assert_eq!(abbreviated_name("T.J. Hockenson"), "T.J. Hockenson");
    }

    #[test]
    fn abbreviated_name_preserves_two_character_first_names() {
        assert_eq!(abbreviated_name("RJ Harvey"), "RJ Harvey");
    }

    #[test]
    fn abbreviated_name_preserves_defense_rows() {
        let team = NflTeamAbbr("KC".into());
        let info = PickInfo::defense(&team, None, &[], None);
        assert!(info.is_dst());
        assert_eq!(info.abbreviated_name(), "KC D/ST");

        let picks = picks();
        let defense = &picks.rows[8];
        assert_eq!(defense.abbreviated_name(), "KC D/ST");
    }

    #[test]
    fn is_long_name_starts_after_fifteen_characters() {
        assert!(!is_long_name("Patrick Mahomes"), "15 chars stays short");
        assert!(is_long_name("Jameson Williams"), "16 chars is long");
    }

    #[test]
    fn preview_rows_carry_salary_and_schedule_never_scores() {
        let f = Fixture::default();
        let (rows, spent) = build_preview(
            &lineup(),
            &f.names,
            &slate(),
            &f.salaries,
            &HashMap::new(),
            &HashMap::new(),
            None,
        );
        assert_eq!(rows.len(), 9, "all nine slots");
        assert_eq!(
            spent,
            rows.iter().filter_map(|r| r.info.salary).sum::<i64>(),
            "spent is the sum of known salaries"
        );
        for row in &rows {
            if let Some(g) = row.info.game() {
                assert_eq!(g.away_score, None, "no game scores before kickoff");
                assert_eq!(g.home_score, None);
            }
        }
    }

    #[test]
    fn entry_page_total_agrees_with_the_standings_total() {
        let picks = picks();
        assert_eq!(
            picks.total,
            Some(scoring::score_lineup(&lineup(), &week_stats()))
        );
    }

    #[test]
    fn unknown_player_renders_raw_id_and_dash_matchup() {
        let mut f = Fixture::default();
        f.names.remove("00-QB");
        let picks = f.picks();
        let qb = &picks.rows[0];
        assert_eq!(qb.name, "00-QB");
        assert_eq!(qb.matchup_str(), "—");
    }

    #[test]
    fn game_line_bolds_the_home_team_when_the_pick_is_home() {
        let g = Game {
            home_score: Some(20),
            away_score: Some(13),
            ..factories::game("2025_01_BUF_KC", 1)
        };
        let line = game_line(&[g], "KC").expect("game found");
        assert_eq!(line.home, "KC");
        assert_eq!(line.away, "BUF");
        assert_eq!(line.home_score, Some(20));
        assert_eq!(line.away_score, Some(13));
        assert!(line.home_is_pick, "KC is the home side");
    }

    #[test]
    fn game_line_marks_away_pick_and_keeps_missing_scores() {
        let line = game_line(&[factories::game("2025_01_BUF_KC", 1)], "BUF").expect("game found");
        assert!(!line.home_is_pick, "BUF is the away side");
        assert_eq!(line.home_score, None);
        assert_eq!(line.away_score, None);
    }

    #[test]
    fn game_line_none_when_team_not_on_slate() {
        assert!(game_line(&[factories::game("2025_01_BUF_KC", 1)], "SF").is_none());
    }

    #[test]
    fn ownership_is_the_share_of_lineups_rostering_a_pick() {
        let a = lineup(); // rb1 = 00-RB1, def = KC
        let mut b = lineup();
        b.rb1 = NflPlayerId("00-OTHER".into());
        b.def = NflTeamAbbr("SF".into());
        let own = compute_ownership(&[a.clone(), b]);
        // 00-QB is in both lineups -> 1.0; 00-RB1 in one of two -> 0.5.
        assert_eq!(own.player(&NflPlayerId("00-QB".into())), Some(1.0));
        assert_eq!(own.player(&NflPlayerId("00-RB1".into())), Some(0.5));
        // KC defense in one of two -> 0.5; a team nobody has -> None.
        assert_eq!(own.defense(&NflTeamAbbr("KC".into())), Some(0.5));
        assert_eq!(own.defense(&NflTeamAbbr("DEN".into())), None);
    }
}
