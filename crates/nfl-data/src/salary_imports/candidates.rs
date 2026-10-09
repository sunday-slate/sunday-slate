use crate::{NflData, Player, Season, TeamAbbr, Week, WeeklyRosterEntry};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Candidate {
    pub gsis_id: String,
    pub full_name: String,
    pub position: Option<String>,
}
impl Candidate {
    pub(crate) fn label(&self) -> String {
        match &self.position {
            Some(position) => format!("{} ({position})", self.full_name),
            None => self.full_name.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CandidateSource {
    Week(Week),
    LatestTeam,
    AllTeamsWeeks,
}
impl CandidateSource {
    pub(crate) fn label(self, team: &str) -> String {
        match self {
            Self::Week(week) => format!("on {team}, week {}", week.0),
            Self::LatestTeam => format!("on {team}, no weekly roster synced"),
            Self::AllTeamsWeeks => "across all teams and weeks".to_owned(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CandidateQuery {
    pub candidates: Vec<Candidate>,
    pub source: CandidateSource,
    pub widened: bool,
    pub too_short: bool,
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

async fn week_candidates(
    nfl: &NflData,
    season: Season,
    week: Week,
    team: &str,
) -> Result<Vec<Candidate>, crate::NflDataError> {
    let roster = nfl
        .weekly_roster(season, week, &TeamAbbr(team.to_owned()))
        .await?;
    Ok(roster
        .iter()
        .filter_map(from_roster)
        .filter(|candidate| is_skill(candidate.position.as_deref()))
        .collect())
}

async fn latest_team_candidates(
    nfl: &NflData,
    team: &str,
) -> Result<Vec<Candidate>, crate::NflDataError> {
    Ok(nfl
        .players()
        .await?
        .iter()
        .filter(|player| {
            player
                .latest_team
                .as_ref()
                .is_some_and(|latest| latest.0 == team)
                && is_skill(player.position.as_deref())
        })
        .map(from_player)
        .collect())
}

async fn team_candidates(
    nfl: &NflData,
    season: Season,
    week: Week,
    team: &str,
) -> Result<(Vec<Candidate>, CandidateSource), crate::NflDataError> {
    let weekly = week_candidates(nfl, season, week, team).await?;
    if !weekly.is_empty() {
        return Ok((weekly, CandidateSource::Week(week)));
    }
    Ok((
        latest_team_candidates(nfl, team).await?,
        CandidateSource::LatestTeam,
    ))
}

async fn all_candidates(nfl: &NflData) -> Result<Vec<Candidate>, crate::NflDataError> {
    Ok(nfl
        .players()
        .await?
        .iter()
        .filter(|player| is_skill(player.position.as_deref()))
        .map(from_player)
        .collect())
}

/// Query the row's team/week roster, or explicitly widen across cached players.
/// The widened player table has a two-character floor; the smaller team list can
/// be filtered with one character and remains visible for an empty query.
pub(crate) async fn query(
    nfl: &NflData,
    season: Season,
    week: Week,
    team: &str,
    query: &str,
    widened: bool,
) -> Result<CandidateQuery, crate::NflDataError> {
    let needle = query.trim().to_lowercase();
    if widened {
        let too_short = needle.len() < 2;
        let candidates = if too_short {
            Vec::new()
        } else {
            all_candidates(nfl)
                .await?
                .into_iter()
                .filter(|candidate| candidate.label().to_lowercase().contains(&needle))
                .collect()
        };
        return Ok(CandidateQuery {
            candidates,
            source: CandidateSource::AllTeamsWeeks,
            widened: true,
            too_short,
        });
    }

    let (candidates, source) = team_candidates(nfl, season, week, team).await?;
    let candidates = if needle.is_empty() {
        candidates
    } else {
        candidates
            .into_iter()
            .filter(|candidate| candidate.label().to_lowercase().contains(&needle))
            .collect()
    };
    Ok(CandidateQuery {
        candidates,
        source,
        widened: false,
        too_short: false,
    })
}

pub(crate) async fn is_eligible(
    nfl: &NflData,
    season: Season,
    week: Week,
    team: &str,
    id: &str,
) -> Result<bool, crate::NflDataError> {
    let (team_candidates, _) = team_candidates(nfl, season, week, team).await?;
    if team_candidates
        .iter()
        .any(|candidate| candidate.gsis_id == id)
    {
        return Ok(true);
    }
    Ok(all_candidates(nfl)
        .await?
        .iter()
        .any(|candidate| candidate.gsis_id == id))
}
