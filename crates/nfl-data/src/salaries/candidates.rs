use crate::salaries::SalaryError;
use crate::{Player, SalariedPlayer, Season, SeasonWeek, TeamAbbr, Week, WeeklyRosterEntry};

pub struct Candidate {
    pub gsis_id: String,
    pub label: String, // "Jaxon Smith-Njigba (WR)"
    /// Sort key only; not rendered.
    pub sort_key: String,
}

/// Surname first (suffixes already stripped), full name breaks ties.
fn sort_key(full_name: &str, last_name: Option<&str>) -> String {
    let last = last_name.unwrap_or(full_name);
    format!("{last} {full_name}").to_lowercase()
}

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

/// Positions a FanDuel salary row can never be; exclusion list so unfamiliar
/// or blank positions still reach the admin.
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

/// Fallback when no weekly roster exists for the row's week.
fn candidates_for(players: &[Player], team: &str) -> Vec<Candidate> {
    players
        .iter()
        .filter(|p| p.latest_team.as_ref().map(|t| t.as_str()) == Some(team))
        .filter(|p| is_skill_position(p.position.as_deref()))
        .map(candidate)
        .collect()
}

/// One weekly-roster entry as a candidate; drops entries without a GSIS id.
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

/// Candidates from the exact week's roster — the trade-correct scope.
async fn roster_candidates(
    nfl: &crate::NflData,
    season: Season,
    week: Week,
    team: &TeamAbbr,
) -> Result<Vec<Candidate>, SalaryError> {
    let entries = nfl.weekly_roster(season, week, team).await?;
    Ok(entries.iter().filter_map(roster_candidate).collect())
}

pub(crate) async fn suggestion_for(
    nfl: &crate::NflData,
    reconciled_gsis_ids: &[String],
    season_week: &SeasonWeek,
    salary: &SalariedPlayer,
) -> Result<Option<Candidate>, SalaryError> {
    let Some(player) = &salary.player else {
        return Ok(None);
    };
    let entries = nfl
        .weekly_roster(season_week.season, season_week.week, &salary.team)
        .await?;
    let index = super::matcher::RosterIndex::build(&entries);
    let name = format!("{} {}", player.first_name, player.last_name);

    let Some(gsis_id) = index.suggest_one(&name, salary.team.as_str(), salary.position.into())
    else {
        return Ok(None);
    };

    if reconciled_gsis_ids.contains(&gsis_id) {
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

pub(crate) async fn search(
    nfl: &crate::NflData,
    reconciled_gsis_ids: &[String],
    season_week: &SeasonWeek,
    team: &TeamAbbr,
    query: &str,
    widened: bool,
) -> Result<CandidateResults, SalaryError> {
    let needle = query.trim().to_lowercase();
    let (mut candidates, scope_label) = if widened {
        let hits: Vec<Candidate> = nfl
            .players()
            .await?
            .iter()
            .filter(|p| p.full_name.to_lowercase().contains(&needle))
            .filter(|p| is_skill_position(p.position.as_deref()))
            .map(candidate)
            .collect();
        (hits, "across all teams".to_owned())
    } else {
        let roster = roster_candidates(nfl, season_week.season, season_week.week, team).await?;
        let (base, label) = if roster.is_empty() {
            let players = nfl.players().await?;
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
    candidates.retain(|candidate| !reconciled_gsis_ids.contains(&candidate.gsis_id));
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
