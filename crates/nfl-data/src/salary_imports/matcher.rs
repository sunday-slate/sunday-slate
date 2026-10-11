use nfl_model::{Player, WeeklyRosterEntry};
use std::collections::HashMap;

fn normalize(name: &str) -> String {
    name.to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == ' ')
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub(crate) struct PlayerIndex {
    by_norm: HashMap<String, Vec<Player>>,
}
impl PlayerIndex {
    pub(crate) fn build(players: &[Player]) -> Self {
        let mut by_norm: HashMap<String, Vec<Player>> = HashMap::new();
        for player in players {
            by_norm
                .entry(normalize(&player.full_name))
                .or_default()
                .push(player.clone());
        }
        Self { by_norm }
    }
    pub(crate) fn match_one(&self, name: &str, team: &str) -> Option<String> {
        let hits = self.by_norm.get(&normalize(name))?;
        match hits.as_slice() {
            [] => None,
            [p] => Some(p.gsis_id.clone()),
            many => {
                let mut teams = many
                    .iter()
                    .filter(|p| p.latest_team.as_ref().map(|t| t.0.as_str()) == Some(team));
                let first = teams.next()?;
                if teams.next().is_none() {
                    Some(first.gsis_id.clone())
                } else {
                    None
                }
            }
        }
    }
}

const SUFFIXES: &[&str] = &["jr", "sr", "ii", "iii", "iv", "v"];
fn strip_suffix(name: &str) -> String {
    let normalized = normalize(name);
    let mut parts: Vec<&str> = normalized.split(' ').collect();
    while parts.len() > 1 && SUFFIXES.contains(parts.last().unwrap()) {
        parts.pop();
    }
    parts.join(" ")
}
struct RosterHit {
    id: String,
    position: Option<String>,
}
pub(crate) struct RosterIndex {
    by_team_name: HashMap<(String, String), Vec<RosterHit>>,
    by_team_stem: HashMap<(String, String), Vec<RosterHit>>,
}
impl RosterIndex {
    pub(crate) fn build(roster: &[WeeklyRosterEntry]) -> Self {
        let mut by_team_name: HashMap<(String, String), Vec<RosterHit>> = HashMap::new();
        let mut by_team_stem: HashMap<(String, String), Vec<RosterHit>> = HashMap::new();
        for entry in roster {
            let Some(id) = entry.gsis_id.clone() else {
                continue;
            };
            let hit = || RosterHit {
                id: id.clone(),
                position: entry.position.clone(),
            };
            by_team_name
                .entry((entry.team.0.clone(), normalize(&entry.full_name)))
                .or_default()
                .push(hit());
            by_team_stem
                .entry((entry.team.0.clone(), strip_suffix(&entry.full_name)))
                .or_default()
                .push(hit());
        }
        Self {
            by_team_name,
            by_team_stem,
        }
    }
    pub(crate) fn match_one(&self, name: &str, team: &str, position: &str) -> Option<String> {
        let exact = self.by_team_name.get(&(team.to_owned(), normalize(name)));
        exact.and_then(|hits| choose(hits, position)).or_else(|| {
            self.by_team_stem
                .get(&(team.to_owned(), strip_suffix(name)))
                .and_then(|hits| choose(hits, position))
        })
    }
    pub(crate) fn suggest_one(&self, name: &str, team: &str, position: &str) -> Option<String> {
        let normalized = strip_suffix(name);
        let surname = normalized.rsplit(' ').next().unwrap_or_default();
        let mut hits = self
            .by_team_stem
            .iter()
            .filter(|((candidate_team, stem), _)| {
                candidate_team == team && stem.rsplit(' ').next() == Some(surname)
            })
            .flat_map(|(_, hits)| hits.iter())
            .filter(|hit| hit.position.as_deref() == Some(position));
        let first = hits.next()?;
        if hits.next().is_none() {
            Some(first.id.clone())
        } else {
            None
        }
    }
}
fn choose(hits: &[RosterHit], position: &str) -> Option<String> {
    match hits {
        [] => None,
        [hit] => Some(hit.id.clone()),
        many => {
            let mut selected = many
                .iter()
                .filter(|hit| hit.position.as_deref() == Some(position));
            let first = selected.next()?;
            if selected.next().is_none() {
                Some(first.id.clone())
            } else {
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nfl_model::{Season, TeamAbbr, Week};

    fn entry(id: &str, name: &str) -> WeeklyRosterEntry {
        WeeklyRosterEntry {
            season: Season(2026),
            week: Week(1),
            team: TeamAbbr("BUF".into()),
            gsis_id: Some(id.into()),
            espn_id: None,
            full_name: name.into(),
            last_name: Some("Guy".into()),
            position: Some("WR".into()),
        }
    }

    #[test]
    fn exact_roster_name_precedes_suffix_stripped_teammates() {
        let index = RosterIndex::build(&[
            entry("00-plain", "Same Guy"),
            entry("00-jr", "Same Guy Jr."),
        ]);

        assert_eq!(
            index.match_one("Same Guy", "BUF", "WR").as_deref(),
            Some("00-plain")
        );
        assert_eq!(
            index.match_one("Same Guy Jr.", "BUF", "WR").as_deref(),
            Some("00-jr")
        );
    }
}
