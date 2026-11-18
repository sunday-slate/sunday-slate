//! The entry page's data: the structs the template renders and their
//! construction.

use crate::injuries::{self, InjuryChip};
use crate::{assets, avatars};
#[cfg(test)]
use nfl_data::DfsPosition;
use nfl_data::Snapshot;
use std::collections::HashMap;
#[cfg(test)]
use std::sync::Arc;
#[cfg(test)]
use time::macros::datetime;

use nfl_data::salaried_players::{SalariedPlayer, WeeklyStats};
use nfl_data::slates::SlateId;
use nfl_data::{
    Game, InjuryDesignation, LiveGamePhase, PlayerIdentity, PlayerWeekStats as NflPlayerWeekStats,
    TeamAbbr,
};

use crate::avatars::Avatars;
use crate::contests::service as contests;
use crate::entries::EntryId;
use crate::entries::store;
use crate::entries::{Lineup, NflPlayerId, PlayerOrDefenseId, RosterSlot};
use crate::fantasy_teams as fantasy_teams_module;
use crate::fantasy_teams::store as fantasy_teams;
use crate::live::{self, ContestScores, Coverage, LiveContestSnapshot};
use crate::media::Media;
use crate::scoring;
use crate::scoring::service as scoring_service;
use crate::{AppError, AppState, User};
use nfl_data::scoring::{self as nfl_scoring, Score, ScoreLine, WeekStats};
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
    pub fn contest_id(&self) -> SlateId {
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
    pub contest_id: SlateId,
    pub contest_name: String,
    pub rank: Option<u32>,
    pub score: ScoreMeta,
    pub picks: Option<Picks>,
}

impl EntryPage {
    /// Up to two initials from the fantasy team name, for the logo fallback.
    pub fn monogram(&self) -> String {
        fantasy_teams_module::monogram(&self.team_name)
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
        .find(|g| g.home_team.as_str() == team || g.away_team.as_str() == team)
        .map(|g| GameLine {
            away: g.away_team.as_str().to_owned(),
            away_score: g.away_score,
            home: g.home_team.as_str().to_owned(),
            home_score: g.home_score,
            home_is_pick: g.home_team.as_str() == team,
        })
}

/// How a row shows its subject: a player photo, a DST team logo, or the
/// silhouette placeholder when a player has no photo.
enum Avatar {
    Headshot(String),
    TeamLogo(String),
    Silhouette,
}

impl Avatar {
    fn url(&self) -> String {
        match self {
            Avatar::Headshot(url) | Avatar::TeamLogo(url) => assets::versioned(url),
            Avatar::Silhouette => assets::versioned(SILHOUETTE),
        }
    }
}

pub(crate) const SILHOUETTE: &str = "/static/img/headshot-placeholder.svg";

/// The salary cap for a nine-slot FanDuel-style lineup.
pub const SALARY_CAP: i64 = 60_000;

/// The placeholder image for a row with no avatar.
pub(crate) fn silhouette_url() -> String {
    assets::versioned(SILHOUETTE)
}

pub struct PickInfo {
    salaried: SalariedPlayer,
    avatar: Avatar,
}

impl PickInfo {
    pub(crate) fn new(salaried: SalariedPlayer, media: Option<&Media>) -> Self {
        let avatar = if salaried.position.is_defense() {
            Avatar::TeamLogo(media.map_or_else(|| SILHOUETTE.to_string(), |media| media.url()))
        } else {
            match media {
                Some(media) => Avatar::Headshot(media.url()),
                None => Avatar::Silhouette,
            }
        };
        Self { salaried, avatar }
    }

    pub fn injury(&self) -> Option<InjuryChip> {
        self.salaried.injury.map(injuries::chip)
    }

    pub fn name(&self) -> &str {
        &self.salaried.name
    }

    /// The pick's price for the slate.
    pub fn salary(&self) -> i64 {
        i64::from(self.salaried.salary)
    }

    /// The pick's game with scores stripped: nothing has been played from the
    /// editor's or preview's point of view.
    pub fn game(&self) -> GameLine {
        let game = &self.salaried.game;
        GameLine {
            away: game.away_team.as_str().to_owned(),
            away_score: None,
            home: game.home_team.as_str().to_owned(),
            home_score: None,
            home_is_pick: game.home_team == self.salaried.team,
        }
    }

    /// `"Sun 11/23 1:00pm"` Eastern, `None` while the game has no kickoff time.
    pub fn kickoff(&self) -> Option<String> {
        self.salaried.game.kickoff_eastern().map(short_kickoff)
    }

    /// The compact row name; DST names return unchanged.
    pub fn abbreviated_name(&self) -> String {
        if self.is_dst() {
            self.name().to_owned()
        } else {
            abbreviated_name(self.name())
        }
    }

    /// Whether the name exceeds the content heuristic for a row.
    pub fn is_long_name(&self) -> bool {
        is_long_name(self.name())
    }

    /// The row's image source.
    pub fn avatar_url(&self) -> String {
        self.avatar.url()
    }

    /// A team logo sits contained and padded; a photo fills the cell.
    pub fn avatar_is_logo(&self) -> bool {
        matches!(self.avatar, Avatar::TeamLogo(_))
    }

    /// Whether this pick represents a DST.
    pub fn is_dst(&self) -> bool {
        self.avatar_is_logo()
    }

    /// `"$5,700"` for the pick's slate price.
    pub fn salary_str(&self) -> String {
        format!("${}", scoring::format_thousands(self.salary()))
    }

    /// The picker's position caption: `"QB"`, `"DST"`, ...
    pub fn badge(&self) -> String {
        String::from(self.salaried.position)
    }

    /// What the picker submits for this pick.
    pub fn value(&self) -> PlayerOrDefenseId {
        match self.salaried.id.as_ref() {
            Some(id) => PlayerOrDefenseId::Player(id.clone()),
            None => PlayerOrDefenseId::Defense(self.salaried.team),
        }
    }

    /// `"18.4"` for the info column.
    pub fn fppg_str(&self) -> String {
        format!("{:.1}", self.salaried.fppg)
    }

    /// `"10"` for the info column.
    pub fn played_str(&self) -> String {
        self.salaried.games_played.to_string()
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
    /// player (passing / rushing / receiving), the defensive line for a DST.
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
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn dom_id(&self) -> String {
        self.dom_id.to_owned()
    }

    pub fn injury(&self) -> Option<InjuryChip> {
        self.injury.map(injuries::chip)
    }

    /// The compact row name; DST names return unchanged.
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
    pub fn breakdown(&self) -> &[ScoreLine] {
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

#[derive(Clone, Copy)]
pub enum PickStats<'a> {
    Raw(&'a WeekStats),
    Stored {
        snapshot: &'a Snapshot,
        slate_id: SlateId,
    },
}

#[derive(Default)]
struct PickFacts {
    team: Option<TeamAbbr>,
    opponent: Option<TeamAbbr>,
    top_stats: String,
    score: Score,
    // Distinguish a missing DST attachment from one with an empty scoring line.
    present: bool,
}

fn player_facts(
    stats: PickStats<'_>,
    slot: RosterSlot,
    id: &NflPlayerId,
    position: Option<&str>,
) -> PickFacts {
    match stats {
        PickStats::Raw(week) => match week.players.get(id) {
            Some(row) => PickFacts {
                team: Some(row.team),
                opponent: row.opponent,
                top_stats: top_stats_line(slot, position, row),
                score: nfl_scoring::score_player(row),
                present: true,
            },
            None => PickFacts::default(),
        },
        PickStats::Stored { snapshot, slate_id } => {
            let value = PlayerOrDefenseId::Player(id.clone());
            match stored_week(snapshot, slate_id, &value) {
                Some((WeeklyStats::Player(row), score)) => PickFacts {
                    team: Some(row.team),
                    opponent: row.opponent,
                    top_stats: top_stats_line(slot, position, row),
                    score: score.clone(),
                    present: true,
                },
                _ => PickFacts::default(),
            }
        }
    }
}

fn defense_facts(stats: PickStats<'_>, team: TeamAbbr) -> PickFacts {
    match stats {
        PickStats::Raw(week) => match week.defenses.get(&team) {
            Some(row) => PickFacts {
                team: Some(row.team),
                opponent: Some(row.opponent),
                top_stats: String::new(),
                score: nfl_scoring::score_defense(row),
                present: true,
            },
            None => PickFacts::default(),
        },
        PickStats::Stored { snapshot, slate_id } => {
            let value = PlayerOrDefenseId::Defense(team);
            match stored_week(snapshot, slate_id, &value) {
                Some((WeeklyStats::Defense(row), score)) => PickFacts {
                    team: Some(row.team),
                    opponent: Some(row.opponent),
                    top_stats: String::new(),
                    score: score.clone(),
                    present: true,
                },
                _ => PickFacts::default(),
            }
        }
    }
}

fn stored_week<'a>(
    snapshot: &'a Snapshot,
    slate_id: SlateId,
    value: &PlayerOrDefenseId,
) -> Option<(&'a WeeklyStats, &'a Score)> {
    let scored = snapshot
        .salaried_player(&slate_id, value)?
        .scored_stats
        .as_ref()?;
    Some((&scored.stats, &scored.score))
}

#[allow(clippy::too_many_arguments)]
pub fn build_picks<'a>(
    lineup: &Lineup,
    names: &HashMap<NflPlayerId, PlayerIdentity>,
    stats: PickStats<'_>,
    slate: &[Game],
    salaried: impl Fn(&PlayerOrDefenseId) -> Option<&'a SalariedPlayer>,
    ownership: &Ownership,
    avatars: &Avatars,
) -> Picks {
    let mut rows: Vec<SlotRow> = lineup
        .player_slots()
        .into_iter()
        .map(|(slot, id)| {
            let known = names.get(id);
            let facts = player_facts(stats, slot, id, known.and_then(|k| k.position.as_deref()));
            let team = facts
                .team
                .map(|team| team.as_str().to_owned())
                .or_else(|| known.and_then(|k| k.team.as_ref().map(|t| t.as_str().to_owned())));
            let value = PlayerOrDefenseId::Player(id.clone());
            let avatar = match avatars.get(&value) {
                Some(media) => Avatar::Headshot(media.url()),
                None => Avatar::Silhouette,
            };
            let points = Some(facts.score.total);
            let row = salaried(&value);
            SlotRow {
                slot: slot.position_label(),
                dom_id: slot.dom_id(),
                name: known.map_or_else(|| id.as_ref().to_string(), |k| k.name.clone()),
                avatar,
                team: team.clone(),
                kickoff: team.as_deref().and_then(|team| team_kickoff(slate, team)),
                opponent: facts.opponent.map(|opponent| opponent.as_str().to_owned()),
                game: team.as_deref().and_then(|t| game_line(slate, t)),
                top_stats: facts.top_stats,
                score: facts.score,
                points,
                score_direction: None,
                score_delta: None,
                coverage: Coverage::Complete,
                phase: None,
                period: None,
                clock: None,
                stale: false,
                salary: row.map(|row| i64::from(row.salary)),
                ownership: ownership.player(id),
                injury: row.and_then(|row| row.injury),
            }
        })
        .collect();

    let facts = defense_facts(stats, lineup.def);
    let def_team = lineup.def.as_str().to_owned();
    let defense_id = PlayerOrDefenseId::Defense(lineup.def);
    let top_stats = if facts.present {
        or_dash(facts.score.stat_line())
    } else {
        String::new()
    };
    let points = Some(facts.score.total);
    let row = salaried(&defense_id);
    rows.push(SlotRow {
        slot: RosterSlot::Def.position_label(),
        dom_id: RosterSlot::Def.dom_id(),
        name: format!("{def_team} DST"),
        avatar: Avatar::TeamLogo(
            avatars
                .get(&defense_id)
                .map_or_else(|| SILHOUETTE.to_string(), |media| media.url()),
        ),
        team: Some(def_team.clone()),
        kickoff: team_kickoff(slate, &def_team),
        opponent: facts.opponent.map(|opponent| opponent.as_str().to_owned()),
        game: game_line(slate, &def_team),
        top_stats,
        score: facts.score,
        points,
        score_direction: None,
        score_delta: None,
        coverage: Coverage::Complete,
        phase: None,
        period: None,
        clock: None,
        stale: false,
        salary: row.map(|row| i64::from(row.salary)),
        ownership: ownership.defense(&lineup.def),
        injury: row.and_then(|row| row.injury),
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
    pub contest_id: SlateId,
    /// Names the page: "Week 12 Lineup".
    pub contest_name: String,
    pub rows: Vec<PreviewRow>,
    spent: i64,
}

impl EntryPreview {
    /// Up to two initials from the fantasy team name, for the logo fallback.
    pub fn monogram(&self) -> String {
        fantasy_teams_module::monogram(&self.team_name)
    }

    /// What the lineup left under the cap, `"$600"`.
    pub fn remaining_str(&self) -> String {
        format!("${}", scoring::format_thousands(SALARY_CAP - self.spent))
    }
}

/// One preview row: the slot and who fills it, `None` when the slate has no
/// priced row for the pick.
pub struct PreviewRow {
    pub slot: &'static str,
    pub info: Option<PickInfo>,
}

/// Preview rows in roster order, plus the total salary spent. Game scores are
/// stripped: nothing has been played from this entry's point of view.
pub fn build_preview(
    lineup: &Lineup,
    player: impl Fn(&NflPlayerId) -> Option<SalariedPlayer>,
    defense: impl Fn(&TeamAbbr) -> Option<SalariedPlayer>,
    avatars: &Avatars,
) -> (Vec<PreviewRow>, i64) {
    let mut rows: Vec<PreviewRow> = lineup
        .player_slots()
        .into_iter()
        .map(|(slot, id)| PreviewRow {
            slot: slot.position_label(),
            info: player(id).map(|salaried| {
                PickInfo::new(
                    salaried,
                    avatars.get(&PlayerOrDefenseId::Player(id.clone())),
                )
            }),
        })
        .collect();

    rows.push(PreviewRow {
        slot: RosterSlot::Def.position_label(),
        info: defense(&lineup.def).map(|salaried| {
            PickInfo::new(
                salaried,
                avatars.get(&PlayerOrDefenseId::Defense(lineup.def)),
            )
        }),
    });

    let spent = rows
        .iter()
        .filter_map(|row| row.info.as_ref())
        .map(PickInfo::salary)
        .sum();
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
/// The team's slate kickoff as `"Sun 11/23 1:00pm"` Eastern, `None` while
/// unscheduled.
pub(crate) fn team_kickoff(slate: &[Game], team: &str) -> Option<String> {
    slate
        .iter()
        .find(|g| g.home_team.as_str() == team || g.away_team.as_str() == team)
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
    shares: HashMap<PlayerOrDefenseId, f64>,
}

impl Ownership {
    pub fn player(&self, id: &NflPlayerId) -> Option<f64> {
        self.shares
            .get(&PlayerOrDefenseId::Player(id.clone()))
            .copied()
    }
    pub fn defense(&self, team: &TeamAbbr) -> Option<f64> {
        self.shares.get(&PlayerOrDefenseId::Defense(*team)).copied()
    }
}

/// Count each pick across all lineups and divide by the lineup count.
pub fn compute_ownership<'a>(lineups: impl IntoIterator<Item = &'a Lineup>) -> Ownership {
    let mut total = 0.0;
    let mut counts: HashMap<PlayerOrDefenseId, u32> = HashMap::new();
    for lineup in lineups {
        total += 1.0;
        for id in lineup.player_ids() {
            *counts
                .entry(PlayerOrDefenseId::Player(id.clone()))
                .or_insert(0) += 1;
        }
        *counts
            .entry(PlayerOrDefenseId::Defense(lineup.def))
            .or_insert(0) += 1;
    }
    let frac = |c: u32| c as f64 / total;
    Ownership {
        shares: counts
            .into_iter()
            .map(|(value, count)| (value, frac(count)))
            .collect(),
    }
}
pub async fn page(state: &AppState, viewer: &User, id: EntryId) -> Result<EntryView, AppError> {
    let reader = state.db.reader();
    let entry = store::by_id(reader, id).await?.ok_or(AppError::NotFound)?;
    fantasy_teams::team_for_user(reader, entry.league_id, viewer.id)
        .await?
        .ok_or(AppError::NotFound)?;

    let snapshot_ref = state.nfl.snapshot();
    let slate = snapshot_ref
        .slate(&entry.slate_id)
        .ok_or(AppError::NotFound)?;
    let games = snapshot_ref.games_for_slate(&entry.slate_id);
    let contest_name = slate.name.clone();
    let now = state.now();

    let is_owner = entry.team.user_id == viewer.id;
    let kicked_off = slate.locked(now);

    if is_owner && !kicked_off {
        let values: Vec<PlayerOrDefenseId> = entry
            .lineup
            .to_slots()
            .into_iter()
            .map(|slot| slot.value)
            .collect();
        let avatars = avatars::by_id(&state.db, &values).await?;
        let (rows, spent) = build_preview(
            &entry.lineup,
            |id| snapshot_ref.player(&entry.slate_id, id).cloned(),
            |team| snapshot_ref.defense(&entry.slate_id, team).cloned(),
            &avatars,
        );
        return Ok(EntryView::Preview(EntryPreview {
            team_name: entry.team.name,
            owner_name: entry.team.owner_name,
            logo_url: entry.team.logo.map(|m| m.url()),
            contest_id: entry.slate_id,
            contest_name,
            rows,
            spent,
        }));
    }

    let (picks, rank, score) = if kicked_off {
        let ids = entry.lineup.player_ids().map(Clone::clone);
        let source = live::load_contest_scores(state, entry.slate_id).await?;
        let stream_eligible = live::stream_eligible(state, entry.slate_id, &source).await?;
        let unsynced = WeekStats::default();
        // Keep official scores, games, and salaries on the same publication.
        let (stats, display_slate, roster) = match &source {
            ContestScores::Official(official) => (
                PickStats::Stored {
                    snapshot: official,
                    slate_id: entry.slate_id,
                },
                official.games_for_slate(&entry.slate_id).to_vec(),
                official.as_ref(),
            ),
            ContestScores::Provisional(snapshot) => (
                PickStats::Raw(&snapshot.stats),
                snapshot.scored_games(),
                snapshot_ref.as_ref(),
            ),
            ContestScores::Upcoming | ContestScores::Unavailable => (
                PickStats::Raw(&unsynced),
                games.to_vec(),
                snapshot_ref.as_ref(),
            ),
        };
        let names = state.nfl.identify(&ids).await?;
        let values: Vec<PlayerOrDefenseId> = entry
            .lineup
            .to_slots()
            .into_iter()
            .map(|slot| slot.value)
            .collect();
        let avatars = avatars::by_id(&state.db, &values).await?;
        let contest_entries = store::by_contest(reader, entry.slate_id, entry.league_id).await?;

        let ownership = compute_ownership(contest_entries.iter().map(|e| &e.lineup));

        let rank = match &source {
            ContestScores::Official(official) => {
                contests::rank_contest_entries(contests::entrants(&contest_entries, |lineup| {
                    Some(scoring_service::lineup_total(
                        lineup,
                        official,
                        entry.slate_id,
                    ))
                }))
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
            stats,
            &display_slate,
            |value| roster.salaried_player(&entry.slate_id, value),
            &ownership,
            &avatars,
        );
        match &source {
            ContestScores::Provisional(snapshot) => {
                apply_live_snapshot(&mut picks, &entry.lineup, snapshot, now);
            }
            ContestScores::Unavailable => clear_live_scores(&mut picks),
            ContestScores::Official(_) | ContestScores::Upcoming => {}
        }
        let score = score_meta(&source, &entry.lineup, id, now, stream_eligible);
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
        contest_id: entry.slate_id,
        contest_name,
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
        Game, PlayerWeekStats as NflPlayerWeekStats, TeamAbbr, TeamWeekStats as NflTeamWeekStats,
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
            def: TeamAbbr::KC,
        }
    }

    /// Every pick named, each on KC.
    fn names() -> HashMap<NflPlayerId, PlayerIdentity> {
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
        .map(|(gsis, name)| {
            (
                NflPlayerId(gsis.to_string()),
                factories::identity(name, Some("KC")),
            )
        })
        .collect()
    }

    /// RB1 with 100 rushing yards (13.00) and the KC defense allowing 3
    /// (7.00); every other slot has no stat row.
    fn week_stats() -> WeekStats {
        let rb = NflPlayerWeekStats {
            opponent: Some(TeamAbbr::BUF),
            ..factories::rb_stats("00-RB1", 1, 100)
        };
        let def = NflTeamWeekStats {
            points_allowed: 3,
            ..factories::defense_stats("KC", "LA", 1)
        };
        WeekStats {
            players: [(NflPlayerId("00-RB1".into()), rb)].into(),
            defenses: [(TeamAbbr::KC, def)].into(),
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
        names: HashMap<NflPlayerId, PlayerIdentity>,
        stats: WeekStats,
        ownership: Ownership,
    }

    impl Default for Fixture {
        fn default() -> Fixture {
            Fixture {
                names: names(),
                stats: week_stats(),
                ownership: compute_ownership(&[lineup()]),
            }
        }
    }

    impl Fixture {
        fn picks(&self) -> Picks {
            self.picks_with(&Avatars::default())
        }

        fn picks_with(&self, avatars: &Avatars) -> Picks {
            build_picks(
                &lineup(),
                &self.names,
                PickStats::Raw(&self.stats),
                &slate(),
                |_| None,
                &self.ownership,
                avatars,
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
            contest_id: SlateId(20260011),
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
    fn rows_preserve_roster_identity() {
        let picks = picks();
        let rows: Vec<(String, &str, &str)> = picks
            .rows
            .iter()
            .map(|r| (r.dom_id(), r.slot, r.name.as_str()))
            .collect();
        assert_eq!(
            rows,
            vec![
                ("qb".to_string(), "QB", "Quinn Back"),
                ("rb1".to_string(), "RB", "Alpha Runner"),
                ("rb2".to_string(), "RB", "Beta Runner"),
                ("wr1".to_string(), "WR", "Wide One"),
                ("wr2".to_string(), "WR", "Wide Two"),
                ("wr3".to_string(), "WR", "Wide Three"),
                ("te".to_string(), "TE", "Tight End"),
                ("flex".to_string(), "FLEX", "Flex Player"),
                ("def".to_string(), "DEF", "KC DST"),
            ]
        );
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
        assert_eq!(picks.rows[8].salary_str(), "—", "DST salary also dashes");
        assert_eq!(picks.rows[8].ownership_str(), "—");
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
        f.stats.defenses = [(TeamAbbr::KC, scoreless)].into();
        let picks = f.picks();
        let def = &picks.rows[8];
        assert_eq!(def.matchup_str(), "KC vs LA");
        assert_eq!(def.top_stats(), "—");
        assert_eq!(def.stat_line_str(), "—");
    }

    #[test]
    fn matchup_renders_dash_with_no_team_and_no_stat_row() {
        let mut f = Fixture::default();
        f.names.insert(
            NflPlayerId("00-QB".into()),
            factories::identity("Quinn Back", None),
        );
        let picks = f.picks();
        assert_eq!(picks.rows[0].matchup_str(), "—");
    }

    #[test]
    fn abbreviated_name_preserves_surnames_and_short_tokens() {
        for (full, abbreviated) in [
            ("Drake Maye", "D. Maye"),
            ("Wan'Dale Robinson", "W. Robinson"),
            ("Jaxon Smith-Njigba", "J. Smith-Njigba"),
            ("Amon-Ra St. Brown Jr.", "A. St. Brown Jr."),
            ("Mononymous-Longer-Than-Fits", "Mononymous-Longer-Than-Fits"),
            ("T.J. Hockenson", "T.J. Hockenson"),
            ("RJ Harvey", "RJ Harvey"),
        ] {
            assert_eq!(abbreviated_name(full), abbreviated, "{full}");
        }
    }

    /// A priced slate row on the KC game, kicking off at 1:00pm Eastern.
    fn salaried(
        id: Option<&str>,
        name: &str,
        position: DfsPosition,
        salary: u32,
        fppg: f64,
        games_played: u8,
    ) -> SalariedPlayer {
        SalariedPlayer {
            id: id.map(|id| NflPlayerId(id.into())),
            game: Arc::new(factories::game_at(
                "2025_01_BUF_KC",
                1,
                datetime!(2025-09-07 13:00 -4),
            )),
            name: name.into(),
            position,
            team: TeamAbbr::KC,
            salary,
            games_played,
            fppg,
            injury: None,
            scored_stats: None,
        }
    }

    #[test]
    fn abbreviated_name_preserves_defense_rows() {
        let info = PickInfo::new(
            salaried(None, "KC DST", DfsPosition::Dst, 3_000, 0.0, 0),
            None,
        );
        assert!(info.is_dst());
        assert_eq!(info.abbreviated_name(), "KC DST");

        let picks = picks();
        let defense = &picks.rows[8];
        assert_eq!(defense.abbreviated_name(), "KC DST");
    }

    #[test]
    fn is_long_name_starts_after_fifteen_characters() {
        assert!(!is_long_name("Patrick Mahomes"), "15 chars stays short");
        assert!(is_long_name("Jameson Williams"), "16 chars is long");
    }

    #[test]
    fn preview_rows_carry_salary_and_schedule_never_scores() {
        let players: HashMap<NflPlayerId, SalariedPlayer> = lineup()
            .player_slots()
            .into_iter()
            .map(|(_, id)| {
                let (fppg, games) = if id.as_ref() == "00-RB1" {
                    (11.0, 2)
                } else {
                    (0.0, 0)
                };
                (
                    id.clone(),
                    salaried(
                        Some(id.as_ref()),
                        id.as_ref(),
                        DfsPosition::Rb,
                        5_000,
                        fppg,
                        games,
                    ),
                )
            })
            .collect();
        let defense = salaried(None, "KC DST", DfsPosition::Dst, 3_000, 6.5, 2);
        let (rows, spent) = build_preview(
            &lineup(),
            |id| players.get(id).cloned(),
            |team| (*team == TeamAbbr::KC).then(|| defense.clone()),
            &Avatars::default(),
        );
        assert_eq!(rows.len(), 9, "all nine slots");
        assert_eq!(
            spent,
            rows.iter()
                .filter_map(|r| r.info.as_ref())
                .map(PickInfo::salary)
                .sum::<i64>(),
            "spent is the sum of known salaries"
        );
        for row in &rows {
            let g = row.info.as_ref().expect("priced row").game();
            assert_eq!(g.away_score, None, "no game scores before kickoff");
            assert_eq!(g.home_score, None);
        }
        // Season pace flows to both position groups.
        let rb1 = rows
            .iter()
            .find(|r| r.slot == RosterSlot::Rb1.position_label())
            .expect("RB1 row")
            .info
            .as_ref()
            .expect("priced row");
        assert_eq!(rb1.fppg_str(), "11.0");
        assert_eq!(rb1.played_str(), "2");
        let def = &rows[8];
        assert_eq!(def.slot, RosterSlot::Def.position_label());
        let def = def.info.as_ref().expect("priced row");
        assert_eq!(def.fppg_str(), "6.5");
        assert_eq!(def.played_str(), "2");
    }

    #[test]
    fn entry_page_total_agrees_with_the_standings_total() {
        let picks = picks();
        assert_eq!(picks.total, Some(20.0));
    }

    #[test]
    fn unknown_player_renders_raw_id_and_dash_matchup() {
        let mut f = Fixture::default();
        f.names.remove(&NflPlayerId("00-QB".into()));
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
        b.def = TeamAbbr::SF;
        let own = compute_ownership(&[a.clone(), b]);
        // 00-QB is in both lineups -> 1.0; 00-RB1 in one of two -> 0.5.
        assert_eq!(own.player(&NflPlayerId("00-QB".into())), Some(1.0));
        assert_eq!(own.player(&NflPlayerId("00-RB1".into())), Some(0.5));
        // KC defense in one of two -> 0.5; a team nobody has -> None.
        assert_eq!(own.defense(&TeamAbbr::KC), Some(0.5));
        assert_eq!(own.defense(&TeamAbbr::DEN), None);
    }
}
