use nfl_data::{Player, SalariedPlayer, Season, SeasonWeek, TeamAbbr, Week, WeeklyRosterEntry};

use crate::nfl_players::store::mapped_gsis_ids;
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
    "DB", "DL", "LB", "OL", "P", "K", "LS", "CB", "S", "SS", "FS", "SAF", "DE", "DT", "NT", "ILB",
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
        .filter(|p| p.latest_team.as_ref().map(|t| t.as_str()) == Some(team))
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
    team: &TeamAbbr,
) -> Result<Vec<Candidate>, AppError> {
    let entries = state
        .nfl
        .weekly_roster(season, week, team)
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;
    Ok(entries.iter().filter_map(roster_candidate).collect())
}

/// A human-confirmed nickname/surname suggestion from the exact week's roster.
pub async fn suggestion_for(
    state: &AppState,
    season_week: &SeasonWeek,
    salary: &SalariedPlayer,
) -> Result<Option<Candidate>, AppError> {
    let Some(player) = &salary.player else {
        return Ok(None);
    };
    let entries = state
        .nfl
        .weekly_roster(season_week.season, season_week.week, &salary.team)
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;
    let index = super::matcher::RosterIndex::build(&entries);
    let name = format!("{} {}", player.first_name, player.last_name);

    let Some(gsis_id) = index.suggest_one(&name, salary.team.as_str(), salary.position.into())
    else {
        return Ok(None);
    };

    if mapped_gsis_ids(state.db.reader()).await?.contains(&gsis_id) {
        return Ok(None);
    }

    Ok(entries
        .iter()
        .find(|entry| entry.gsis_id.as_deref() == Some(gsis_id.as_str()))
        .and_then(roster_candidate))
}

pub struct CandidateResults {
    pub candidates: Vec<Candidate>,
    pub total: usize,
    pub scope_label: String,
    pub widened: bool,
}

/// Keep the week's team roster as the default; widening is always explicit.
pub async fn search(
    state: &AppState,
    season_week: &SeasonWeek,
    team: &TeamAbbr,
    query: &str,
    widened: bool,
) -> Result<CandidateResults, AppError> {
    let needle = query.trim().to_lowercase();
    let (mut candidates, scope_label) = if widened {
        let hits: Vec<Candidate> = state
            .nfl
            .players()
            .await
            .map_err(|e| AppError::Internal(e.to_string()))?
            .iter()
            .filter(|p| p.full_name.to_lowercase().contains(&needle))
            .filter(|p| is_skill_position(p.position.as_deref()))
            .map(candidate)
            .collect();
        (hits, "across all teams".to_owned())
    } else {
        let roster = roster_candidates(state, season_week.season, season_week.week, team).await?;
        let (base, label) = if roster.is_empty() {
            let players = state
                .nfl
                .players()
                .await
                .map_err(|e| AppError::Internal(e.to_string()))?;
            (
                candidates_for(&players, team.as_str()),
                format!("on {team}"),
            )
        } else {
            (roster, format!("on {team}"))
        };
        let hits: Vec<Candidate> = base
            .into_iter()
            .filter(|c| c.label.to_lowercase().contains(&needle))
            .collect();
        (hits, label)
    };
    let mapped = mapped_gsis_ids(state.db.reader()).await?;
    candidates.retain(|candidate| !mapped.contains(&candidate.gsis_id));
    let total = candidates.len();
    candidates.sort_by(|a, b| a.sort_key.cmp(&b.sort_key));
    candidates.truncate(100);
    Ok(CandidateResults {
        candidates,
        total,
        scope_label,
        widened,
    })
}

#[cfg(test)]
mod tests {
    use super::is_skill_position;

    #[test]
    fn kicker_position_is_not_a_salary_player_candidate() {
        assert!(!is_skill_position(Some("K")));
    }
}
