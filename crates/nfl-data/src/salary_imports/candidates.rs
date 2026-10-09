use crate::{NflData, Player, Season, TeamAbbr, Week, WeeklyRosterEntry};

#[derive(Debug, Clone)]
pub(crate) struct Candidate {
    pub gsis_id: String,
    pub full_name: String,
    pub position: Option<String>,
}
const NON_SKILL: &[&str] = &[
    "DB", "DL", "LB", "OL", "P", "LS", "CB", "S", "SS", "FS", "SAF", "DE", "DT", "NT", "ILB",
    "MLB", "OLB", "OT", "T", "G", "OG", "C",
];
pub(crate) fn is_skill(position: Option<&str>) -> bool {
    position.is_none_or(|p| !NON_SKILL.contains(&p))
}
fn from_player(player: &Player) -> Candidate {
    Candidate {
        gsis_id: player.gsis_id.clone(),
        full_name: player.full_name.clone(),
        position: player.position.clone(),
    }
}
fn from_roster(entry: &WeeklyRosterEntry) -> Option<Candidate> {
    Some(Candidate {
        gsis_id: entry.gsis_id.clone()?,
        full_name: entry.full_name.clone(),
        position: entry.position.clone(),
    })
}

pub(crate) async fn eligible_players(
    nfl: &NflData,
    season: Season,
    week: Week,
    team: &str,
) -> Result<Vec<Candidate>, crate::NflDataError> {
    let roster = nfl
        .weekly_roster(season, week, &TeamAbbr(team.to_owned()))
        .await?;
    let mut candidates: Vec<Candidate> = roster
        .iter()
        .filter_map(from_roster)
        .filter(|c| is_skill(c.position.as_deref()))
        .collect();
    if candidates.is_empty() {
        candidates = nfl
            .players()
            .await?
            .iter()
            .filter(|p| {
                p.latest_team.as_ref().is_some_and(|t| t.0 == team)
                    && is_skill(p.position.as_deref())
            })
            .map(from_player)
            .collect();
    }
    Ok(candidates)
}

pub(crate) async fn is_eligible(
    nfl: &NflData,
    season: Season,
    week: Week,
    team: &str,
    id: &str,
) -> Result<bool, crate::NflDataError> {
    let mut candidates = eligible_players(nfl, season, week, team).await?;
    if candidates.iter().any(|candidate| candidate.gsis_id == id) {
        return Ok(true);
    }
    // Explicit widened-search picks may include another skill player even when the local roster exists.
    candidates = nfl
        .players()
        .await?
        .iter()
        .filter(|p| is_skill(p.position.as_deref()))
        .map(from_player)
        .collect();
    if candidates.iter().any(|candidate| candidate.gsis_id == id) {
        return Ok(true);
    }
    let roster = nfl
        .weekly_roster(season, week, &TeamAbbr(team.to_owned()))
        .await?;
    Ok(roster
        .iter()
        .filter_map(from_roster)
        .any(|candidate| candidate.gsis_id == id && is_skill(candidate.position.as_deref())))
}
