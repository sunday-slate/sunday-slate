use std::collections::HashMap;

use askama::Template;
use nfl_data::{Player, Season, TeamAbbr as NflTeamAbbr, Week, WeeklyRosterEntry};

use crate::admin::salary_imports::model::SalaryImportRow;
use crate::admin::salary_imports::store as import_store;
use crate::{AppError, AppState};

pub struct Candidate {
    pub gsis_id: String,
    pub label: String, // "Jaxon Smith-Njigba (WR)"
    /// Not rendered — orders the list by surname. See [`sort_key`].
    pub sort_key: String,
}

/// Surname first so lists read the way an admin scans them, then the full name
/// to break ties between players who share one. Upstream has already stripped
/// generational suffixes, so "Dante Fowler Jr." sorts under F, not J. Falls back
/// to the full name when upstream leaves the surname blank.
fn sort_key(full_name: &str, last_name: Option<&str>) -> String {
    let last = last_name.unwrap_or(full_name);
    format!("{last} {full_name}").to_lowercase()
}

/// One player, labeled with position when known.
pub fn candidate(p: &Player) -> Candidate {
    Candidate {
        gsis_id: p.gsis_id.clone(),
        label: match &p.position {
            Some(pos) => format!("{} ({})", p.full_name, pos),
            None => p.full_name.clone(),
        },
        sort_key: sort_key(&p.full_name, p.last_name.as_deref()),
    }
}

/// Positions a FanDuel salary row can never be. Listed as an exclusion rather
/// than an allow-list so an unfamiliar or blank position still reaches the
/// admin instead of being silently dropped. Covers both vocabularies: the
/// weekly roster's coarse buckets (DB/OL/DL) and the finer-grained `players`
/// table (CB/OT/DE).
const NON_SKILL_POSITIONS: &[&str] = &[
    "DB", "DL", "LB", "OL", "P", "LS", "CB", "S", "SS", "FS", "SAF", "DE", "DT", "NT", "ILB",
    "MLB", "OLB", "OT", "T", "G", "OG", "C",
];

pub fn is_skill_position(position: Option<&str>) -> bool {
    match position {
        Some(p) => !NON_SKILL_POSITIONS.contains(&p),
        None => true,
    }
}

/// Team-scoped candidates by the player table's latest team. Used only when no
/// weekly roster has been synced for the row's week — see [`roster_candidates`].
fn candidates_for(players: &[Player], team: &str) -> Vec<Candidate> {
    players
        .iter()
        .filter(|p| p.latest_team.as_ref().map(|t| t.0.as_str()) == Some(team))
        .filter(|p| is_skill_position(p.position.as_deref()))
        .map(candidate)
        .collect()
}

/// One weekly-roster entry as a pickable candidate. Entries upstream leaves
/// without a GSIS id can't be resolved to, so they're dropped.
fn roster_candidate(e: &WeeklyRosterEntry) -> Option<Candidate> {
    if !is_skill_position(e.position.as_deref()) {
        return None;
    }
    Some(Candidate {
        gsis_id: e.gsis_id.clone()?,
        label: match &e.position {
            Some(pos) => format!("{} ({})", e.full_name, pos),
            None => e.full_name.clone(),
        },
        sort_key: sort_key(&e.full_name, e.last_name.as_deref()),
    })
}

/// Who was actually on this team in this week. This is the scoping that makes
/// mid-season trades come out right: 422 players changed teams in 2025 alone,
/// and the player table records only where each of them ended up.
async fn roster_candidates(
    state: &AppState,
    season: Season,
    week: Week,
    team: &str,
) -> Result<Vec<Candidate>, AppError> {
    let entries = state
        .nfl
        .weekly_roster(season, week, &NflTeamAbbr(team.to_string()))
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;
    Ok(entries.iter().filter_map(roster_candidate).collect())
}

/// gsis_game_id -> week, so a staged row can name its own week.
async fn week_by_game(state: &AppState, season: Season) -> Result<HashMap<String, Week>, AppError> {
    let games = state
        .nfl
        .games(season)
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;
    Ok(games
        .into_iter()
        .map(|g| (g.gsis_game_id, g.week))
        .collect())
}

/// Longest candidate list rendered at once. Week-scoped rosters land well under
/// this, so the cap only bites on a broad search or the latest-team fallback;
/// the count line in `_results.html` discloses whatever it hides.
const RESULT_LIMIT: usize = 100;

/// The clickable candidate list for one row: each hit submits its gsis id back
/// to that row's resolve endpoint.
#[derive(Template)]
#[template(path = "admin/salary_imports/_results.html")]
pub struct ResultsFragment {
    import_id: i64,
    row_id: i64,
    candidates: Vec<Candidate>,
    total: usize,
    /// What the list was drawn from, e.g. "on BUF, week 1". Rendered in the
    /// count line so a widened list can never be mistaken for the week's.
    scope_label: String,
    widen_url: String,
    /// Back to this row's own list. Rendered only once widened, which is the
    /// only state the admin cannot otherwise leave: the widen control is gone,
    /// and a query under the global floor returns nothing to pick from.
    narrow_url: String,
    /// True once the admin has widened, which retires the widen control.
    widened: bool,
    /// True when the widened query is under the global two-character floor, so
    /// no search ran at all. Distinguished from an empty result: the admin has
    /// to be told to type more, not that his player does not exist.
    too_short: bool,
}

impl ResultsFragment {
    /// Builds both urls itself rather than taking them: they differ only by
    /// `scope=all`, and a caller free to pass one without the other could
    /// render a way back that widens again.
    pub fn new(
        import_id: i64,
        row_id: i64,
        mut candidates: Vec<Candidate>,
        scope_label: String,
        widened: bool,
        too_short: bool,
    ) -> Self {
        let total = candidates.len();
        // Sorted here, not per source, so every list the admin sees is ordered
        // the same way — and so the cap keeps the first N by surname rather
        // than whatever order the query happened to return.
        candidates.sort_by(|a, b| a.sort_key.cmp(&b.sort_key));
        candidates.truncate(RESULT_LIMIT);
        let narrow_url = format!("/admin/salary-imports/{import_id}/rows/{row_id}/players");
        Self {
            import_id,
            row_id,
            candidates,
            total,
            scope_label,
            widen_url: format!("{narrow_url}?scope=all"),
            narrow_url,
            widened,
            too_short,
        }
    }
}

/// Which scoping a row's default candidate list was actually built from. The
/// caption has to name the list the admin is looking at, and the fallback below
/// fires on a condition — an empty weekly roster — that the caller cannot see.
/// Reported rather than re-derived, so caption and list cannot drift apart.
pub enum CandidateSource {
    /// That team's roster for the row's week.
    Week(Week),
    /// The player table's latest-team scoping, used when the row's week has no
    /// synced roster. Not week-scoped: a player traded away mid-season is
    /// missing from it and a player signed later is in it.
    LatestTeam,
}

impl CandidateSource {
    /// How the count line names this scoping.
    pub fn label(&self, team: &str) -> String {
        match self {
            Self::Week(week) => format!("on {team}, week {}", week.0),
            Self::LatestTeam => format!("on {team}, no weekly roster synced"),
        }
    }
}

/// The list a row shows before anything is typed: that week's roster for the
/// row's team, or the latest-team scoping if no weekly roster is synced.
pub async fn team_default_candidates(
    state: &AppState,
    import_id: i64,
    row: &SalaryImportRow,
) -> Result<(Vec<Candidate>, CandidateSource), AppError> {
    let Some(batch) = import_store::get(state.db.reader(), import_id).await? else {
        return Err(AppError::NotFound);
    };
    let season = Season(batch.season as u16);
    if let Some(week) = week_for_row(state, import_id, row).await? {
        let c = roster_candidates(state, season, week, &row.fd_team).await?;
        if !c.is_empty() {
            return Ok((c, CandidateSource::Week(week)));
        }
    }
    let all = state
        .nfl
        .players()
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;
    Ok((
        candidates_for(&all, &row.fd_team),
        CandidateSource::LatestTeam,
    ))
}

/// The week this row's game belongs to, or `None` when the games table has never
/// heard of it. Both the default candidate list and the scope label it is
/// captioned with have to agree on this, so they read it from one place.
async fn week_for_row(
    state: &AppState,
    import_id: i64,
    row: &SalaryImportRow,
) -> Result<Option<Week>, AppError> {
    let Some(batch) = import_store::get(state.db.reader(), import_id).await? else {
        return Err(AppError::NotFound);
    };
    let weeks = week_by_game(state, Season(batch.season as u16)).await?;
    Ok(weeks.get(&row.gsis_game_id).copied())
}
